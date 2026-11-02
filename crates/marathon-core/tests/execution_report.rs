//! Port of core/src/test/kotlin/com/malinskiy/marathon/report/ExecutionReportTest.kt
mod event_support;
use event_support::*;
use marathon_core::{model::TestStatus, report::ExecutionReport};
use std::sync::Arc;
fn without_retries() -> ExecutionReport {
    report(vec![
        event(Arc::new(result("test1", TestStatus::Incomplete)), true, 0),
        event(Arc::new(result("test2", TestStatus::Passed)), true, 0),
        event(Arc::new(result("test3", TestStatus::Failure)), true, 0),
    ])
}
fn with_retries() -> ExecutionReport {
    report(vec![
        event(Arc::new(result("test2", TestStatus::Failure)), false, 0),
        event(Arc::new(result("test2", TestStatus::Failure)), false, 0),
        event(Arc::new(result("test2", TestStatus::Passed)), true, 0),
        event(Arc::new(result("test3", TestStatus::Failure)), false, 0),
        event(Arc::new(result("test3", TestStatus::Failure)), false, 0),
        event(Arc::new(result("test3", TestStatus::Failure)), true, 0),
    ])
}
#[test]
fn without_retries_should_not_include_the_incomplete_test() {
    assert_eq!(
        without_retries()
            .summary()
            .pools
            .iter()
            .flat_map(|p| &p.tests)
            .filter(|t| t.status == TestStatus::Incomplete)
            .count(),
        0
    );
}
#[test]
fn without_retries_should_include_1_passed_test() {
    assert_eq!(
        without_retries()
            .summary()
            .pools
            .iter()
            .flat_map(|p| &p.tests)
            .filter(|t| t.status == TestStatus::Passed)
            .count(),
        1
    );
}
#[test]
fn without_retries_should_include_1_failed_test() {
    assert_eq!(
        without_retries()
            .summary()
            .pools
            .iter()
            .flat_map(|p| &p.tests)
            .filter(|t| t.status == TestStatus::Failure)
            .count(),
        1
    );
}
#[test]
fn with_retries_should_include_only_one_instance_of_test2_and_it_s_passed() {
    let report = with_retries();
    let tests: Vec<_> = report
        .summary()
        .pools
        .iter()
        .flat_map(|p| &p.tests)
        .filter(|r| r.test.method == "test2".into())
        .collect();
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0].status, TestStatus::Passed);
}
#[test]
fn with_retries_should_include_only_one_instance_of_test3_and_it_s_failed() {
    let report = with_retries();
    let tests: Vec<_> = report
        .summary()
        .pools
        .iter()
        .flat_map(|p| &p.tests)
        .filter(|r| r.test.method == "test3".into())
        .collect();
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0].status, TestStatus::Failure);
}
