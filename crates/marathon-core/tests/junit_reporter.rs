//! Port of core/src/test/kotlin/com/malinskiy/marathon/report/JUnitReporterTest.kt.
mod event_support;
mod reporter_support;
use reporter_support::*;
use std::{
    path::{Path, PathBuf},
    process::Command,
};
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/core/output/tests/myPool/xxyyzz")
        .join(format!("marathon_junit_report_{name}.xml"))
}
fn validate(path: &Path) {
    let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/core/junit/junit-10.xsd");
    let output = checked(
        Command::new("xmllint")
            .args(["--nonet", "--noout", "--schema"])
            .arg(schema)
            .arg(path)
            .output(),
    );
    assert!(
        output.status.success(),
        "XSD validation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn canonical(path: &Path) -> Vec<u8> {
    let output = checked(
        Command::new("xmllint")
            .args(["--nonet", "--noblanks", "--c14n"])
            .arg(path)
            .output(),
    );
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}
fn parity(id: &str, expected: &str) {
    let (temp, _, _) = generate(id);
    let actual = temp.0.join("tests/myPool/marathon_junit_report.xml");
    assert_eq!(canonical(&actual), canonical(&fixture(expected)));
    validate(&actual);
    let jvm = row(id);
    assert_eq!(
        checked(std::fs::read(actual)),
        bytes(&jvm["files"]["tests/myPool/marathon_junit_report.xml"])
    );
}
#[test]
fn only_passed_tests_and_without_retries_should_generate_correct_report() {
    parity("passed", "passed_tests");
}
#[test]
fn only_failed_tests_and_without_retries_should_generate_correct_report() {
    parity("failed", "failed_tests");
}
#[test]
fn with_failure_ignored_incomplete_and_assumption_failure_tests_with_stack_trace_should_generate_correct_report() {
    parity("traces", "failed_tests_with_stacktrace");
}
#[test]
fn with_multiple_execution_of_the_same_test_from_failure_to_passed_should_generate_correct_report() {
    parity("fail-pass", "failed_to_passed_test");
}
#[test]
fn with_multiple_execution_of_the_same_test_from_passed_to_failure_should_generate_correct_report() {
    parity("pass-fail", "passed_to_failed_test");
}
#[test]
fn sample_android_junit_xml_is_valid() {
    validate(&fixture("android_sample"));
}

#[test]
fn junit_create_new_file_and_stream_open_keep_distinct_java_errors() {
    use marathon_core::{
        executor::CoreExecutor,
        report::{JUnitReporter, Reporter},
    };
    for line in include_str!("../../../fixtures/oracle/reporters/junit-errors.jsonl").lines() {
        let expected: serde_json::Value = checked(serde_json::from_str(line));
        let temp = Temp::new();
        let output = temp.0.join("out");
        if expected["id"] == "blocked-parent" {
            checked(std::fs::write(&output, b"blocker"));
        } else {
            checked(std::fs::create_dir_all(
                output.join("tests/myPool/marathon_junit_report.xml"),
            ));
        }
        let path = jcompat::file_manager::JavaPath::from_text(output.to_str().unwrap());
        let failure = checked(CoreExecutor::run(move |core| {
            Box::pin(async move {
                let reporter = JUnitReporter::new(core.vendor(), path, jcompat::file_manager::JavaPath::from_text(""));
                let report = reporter_support::report(&row("passed")["input"]);
                let task = core.vendor().spawn(Box::pin(async move {
                    reporter.generate(&report).await.map_err(|e| match e {
                        marathon_core::error::OperationError::Failed(f) => marathon_core::error::VendorError::Other(f),
                        marathon_core::error::OperationError::Cancelled(c) => {
                            marathon_core::error::VendorError::Cancelled(c)
                        }
                    })
                }));
                match task.join().await {
                    Err(e) => Ok(e.failure().clone()),
                    Ok(()) => panic!("filesystem failure was ignored"),
                }
            })
        }));
        assert_eq!(failure.class, text(&expected["error"]["class"]));
        assert_eq!(
            failure
                .message
                .unwrap()
                .to_string_lossy()
                .replace(temp.0.to_str().unwrap(), "<out>"),
            expected["error"]["message"].as_str().unwrap()
        );
    }
}
