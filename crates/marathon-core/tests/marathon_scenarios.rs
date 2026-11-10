//! Port of core/src/test/kotlin/com/malinskiy/marathon/scenario/SuccessScenariosTest.kt,
//! scenario/SuccessParseScenarioTest.kt, scenario/DeviceFilteringScenarioTest.kt,
//! scenario/DisconnectingScenariosTest.kt, scenario/InvalidConfigScenariosTest.kt,
//! scenario/TwoPoolsDisconnectScenariosTest.kt and scenario/UncompletedScenariosTest.kt.
//!
//! Each runs the production run graph with the Stub vendor on the virtual executor, from
//! its version-1 input in tools/oracle/scenarios. raw.json keeps the source's LENIENT
//! comparison with the copied fixture; exact JVM bytes are in run_flow_oracle.rs.
use marathon_test_support::{assert::should_be_equal_to_as_json, scenario};

fn fixture(name: &str) -> String {
    let path = format!("{}/../../fixtures/core/output/raw/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn raw_json_matches(input: &str, fixture_name: &str) {
    let outcome = scenario::run(input, |_| {});
    assert!(outcome.completed, "{input}: the run did not complete");
    assert!(
        matches!(outcome.value, Some(Ok(_))),
        "{input}: {:?}",
        outcome
            .value
            .as_ref()
            .map(|v| v.as_ref().map_err(|e| e.failure().to_string().to_owned()))
    );
    should_be_equal_to_as_json(&outcome.read("test_result/raw.json"), &fixture(fixture_name));
}

#[test]
fn one_healthy_device_execution_of_one_test_should_pass() {
    raw_json_matches("success.json", "success_scenario_1.json");
}

#[test]
fn one_healthy_device_execution_of_the_parse_command_should_pass() -> Result<(), Box<dyn std::error::Error>> {
    let outcome = scenario::run("parse.json", |_| {});
    assert!(outcome.completed);
    assert!(matches!(outcome.value, Some(Ok(true))));
    let content = outcome.read("parse_result.yml");
    marathon_test_support::yaml::same_data(
        &content,
        "---\ntests:\n- pkg: \"test\"\n  clazz: \"SimpleTest\"\n  method: \"test\"\n  metaProperties: []",
    )?;
    marathon_test_support::yaml::same_data(
        &content,
        outcome.expect["files"]["parse_result.yml"]
            .as_str()
            .expect("oracle YAML"),
    )?;
    Ok(())
}

#[test]
fn one_allowlisted_device_and_empty_blocklist_executing_two_tests_should_pass_on_one_device() {
    raw_json_matches("filtering-2.json", "device_filtering_2.json");
}

#[test]
fn one_blocklisted_device_and_one_allowlisted_executing_two_tests_should_pass_on_one_device() {
    raw_json_matches("filtering-3.json", "device_filtering_3.json");
}

/// Supplemental: the unannotated `DeviceFilteringScenarioTest.kt:30` function (no matrix row).
#[test]
fn one_blocklisted_device_and_empty_allowlist_executing_two_tests_should_pass_on_one_device() {
    raw_json_matches("filtering-1.json", "device_filtering_1.json");
}

#[test]
fn two_healthy_devices_on_execution_of_two_tests_while_one_device_disconnects_should_pass() {
    raw_json_matches("disconnect.json", "disconnecting_scenario_1.json");
}

#[test]
fn all_devices_lost_during_execution_should_fail_instead_of_hanging() {
    // D48: the Kotlin test's 60 s no longer reach the end of the pool's wait, which is that long too.
    let outcome = scenario::run_longer("all-lost.json", 60_000, |_| {});
    assert!(outcome.completed);
    assert!(matches!(outcome.value, Some(Ok(false))));
}

#[test]
fn one_healthy_device_and_invalid_marathon_config_should_fail() {
    let outcome = scenario::run("invalid-config.json", |_| {});
    assert!(outcome.completed);
    match outcome.value {
        Some(Err(e)) => assert_eq!(e.failure().kind.description().to_owned(), "configuration rejected"),
        other => panic!("expected ConfigurationException, got {:?}", other.map(|v| v.is_ok())),
    }
}

#[test]
fn two_devices_in_separate_pools_disconnecting_one_after_another_should_terminate_the_run() {
    // D48: as above.
    let outcome = scenario::run_longer("two-pools-disconnect.json", 60_000, |_| {});
    assert!(outcome.completed);
}

#[test]
fn one_device_that_never_completes_tests_with_100_uncompleted_tests_executed_should_return() {
    raw_json_matches("uncompleted-1.json", "uncompleted_scenario_1.json");
}

#[test]
fn one_device_that_never_completes_tests_with_100_uncompleted_tests_while_throwing_exception_should_return() {
    raw_json_matches("uncompleted-2.json", "uncompleted_scenario_1.json");
}

#[test]
fn one_device_that_never_completes_tests_after_all_retries_should_report_test_as_failed() {
    raw_json_matches("uncompleted-3.json", "uncompleted_scenario_2.json");
}

#[test]
fn one_device_that_never_completes_tests_after_all_retries_with_retry_strategy_should_report_test_as_failed() {
    raw_json_matches("uncompleted-4.json", "uncompleted_scenario_2.json");
}
