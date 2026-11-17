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
        d78_native_junit(
            &jvm["input"],
            "myPool",
            &bytes(&jvm["files"]["tests/myPool/marathon_junit_report.xml"])
        )
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

/// D78: the test case of `com.example#test1` whose runs are `(status, final, stack trace)`, built from the
/// `fail-pass` row's events.
fn d78_case(runs: &[(&str, bool, Option<&str>)]) -> String {
    use marathon_core::report::junit_xml;
    let mut input = row("fail-pass")["input"].clone();
    let template = input["tests"][1].clone();
    input["tests"] = runs
        .iter()
        .map(|&(status, last, trace)| {
            let mut event = template.clone();
            event["final"] = last.into();
            let result = event["result"].as_object_mut().unwrap();
            result.insert("status".into(), status.into());
            result.remove("stacktrace");
            if let Some(trace) = trace {
                result.insert("stacktrace".into(), trace.into());
            }
            event
        })
        .collect();
    let report = report(&input);
    let pool = report
        .summary()
        .pools
        .iter()
        .find(|p| p.pool_id.name == "myPool")
        .unwrap();
    let xml = junit_xml(pool);
    xml[xml.find("<testcase").unwrap()..xml.rfind("</testsuite>").unwrap()].to_owned()
}
const CASE: &str = "<testcase name=\"test1\" time=\"11.919\" classname=\"com.example\">";

/// D78: a test that passed every run (`flakinessStrategy: fixed`, say) is not flaky, so it lists no runs. Kotlin
/// listed each extra passing run as an empty `flakyFailure`.
#[test]
fn passed_test_whose_copies_all_passed_has_no_flaky_failure() {
    let case = d78_case(&[
        ("PASSED", false, None),
        ("PASSED", false, Some("")),
        ("PASSED", true, None),
    ]);
    assert_eq!(case, format!("{CASE}</testcase>"));
}

/// D78: a passed test lists its failed run, with its trace, and not its other passing run.
#[test]
fn passed_test_with_one_failed_copy_has_exactly_that_flaky_failure() {
    let case = d78_case(&[
        ("PASSED", false, None),
        ("FAILURE", false, Some("boom")),
        ("PASSED", true, None),
    ]);
    assert_eq!(
        case,
        format!(
            "{CASE}<flakyFailure type=\"rerun exception\"><stackTrace><![CDATA[boom]]></stackTrace></flakyFailure>\
             </testcase>"
        )
    );
}

/// A failed test lists only its other failed runs as `rerunFailure`, as Kotlin did; D78 leaves this branch as it was.
#[test]
fn failed_test_with_mixed_copies_has_rerun_failure_only_for_failed_runs() {
    let case = d78_case(&[
        ("PASSED", false, Some("p")),
        ("FAILURE", false, Some("f1")),
        ("PASSED", false, None),
        ("FAILURE", true, Some("last")),
    ]);
    assert_eq!(
        case,
        format!(
            "{CASE}<failure><![CDATA[last]]></failure><rerunFailure type=\"rerun exception\"><stackTrace>\
             <![CDATA[f1]]></stackTrace></rerunFailure></testcase>"
        )
    );
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
        let path = std::path::PathBuf::from(output.to_str().unwrap());
        let failure = checked(CoreExecutor::run(move |core| {
            Box::pin(async move {
                let reporter = JUnitReporter::new(core.vendor(), path, std::path::PathBuf::from(""));
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
        assert_eq!(
            failure.kind,
            marathon_test_support::recorded_errors::kind(&text(&expected["error"]["class"]))
        );
        assert_eq!(
            failure
                .message
                .unwrap()
                .to_owned()
                .replace(temp.0.to_str().unwrap(), "<out>"),
            expected["error"]["message"].as_str().unwrap()
        );
    }
}

#[test]
fn junit_rejects_nul_pool_before_creating_directories() {
    use marathon_core::{
        executor::CoreExecutor,
        report::{JUnitReporter, Reporter},
    };
    let temp = Temp::new();
    let output = temp.0.join("out");
    let path = output.clone();
    let mut input = row("passed")["input"].clone();
    for key in ["connected", "tests"] {
        for event in input[key].as_array_mut().unwrap() {
            event["pool"] = "pool\0tail".into();
        }
    }
    let failure = checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let reporter = JUnitReporter::new(core.vendor(), path, PathBuf::new());
            let report = reporter_support::report(&input);
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
                Ok(()) => panic!("NUL pool was accepted"),
            }
        })
    }));
    assert_eq!(failure.kind, marathon_error::FailureKind::InvalidPath);
    assert_eq!(failure.message.as_deref(), Some("Nul character not allowed"));
    assert!(!output.exists());
}
