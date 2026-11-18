//! Port of vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/MacosDevice.kt
//! D87: a Mac's preparation budget counts every command with a limit that a preparation can send. Each variant runs a whole
//! preparation on the fake worker along its longest path (the installer's three attempts at the test bundle, an arm64e
//! Mac whose runner loses an architecture, the probe finding XCUITest) and compares the commands sent, all under `shell`,
//! with the budget: equal where the variant's longest path is the one the budget counts, never more.
mod support;
use marathon_apple::apple_device::AppleDevice;
use marathon_config::vendor::{MacosPermission, TestType};
use marathon_core::vendor::Device;
use marathon_test_support::apple::fake_simulator_worker::ok;
use std::sync::{Arc, atomic::Ordering};
use support::*;

const SHELL: i64 = 30_000;

/// What a whole preparation of the variant sends, and the budget the device gives it.
async fn variant(test_type: Option<TestType>, permissions: usize, coverage: bool) -> R<(i64, i64, Vec<String>)> {
    let mut h = Harness::new("")?;
    {
        let vendor = Arc::make_mut(&mut h.options.vendor);
        if let Some(bundle) = vendor.bundle.as_mut() {
            bundle.test_type = test_type;
        }
        if permissions > 0 {
            vendor.permissions.grant = vec![(
                "com.example".into(),
                [MacosPermission::Accessibility, MacosPermission::DeveloperTool][..permissions].to_vec(),
            )];
        }
        assert_eq!(vendor.timeout_configuration.shell.to_millis(), Some(SHELL));
    }
    Arc::make_mut(&mut h.options.dependencies.configuration).is_code_coverage_enabled = coverage;
    // The longest path: an arm64e Mac whose runner has an architecture the tests lack, and a probe finding XCUITest.
    let answer = h.worker.fake.before.lock().unwrap().clone();
    *h.worker.fake.before.lock().unwrap() = Arc::new(move |cmd: Vec<String>| {
        let reply = match cmd.first().map(String::as_str) {
            Some("uname") => Some(ok(["arm64e"])),
            Some("lipo") if cmd.iter().any(|s| s == "-archs") => Some(match cmd[1].contains("Runner") {
                true => ok(["arm64 arm64e"]),
                false => ok(["arm64"]),
            }),
            Some("sh") if cmd.iter().any(|s| s.contains("nm ")) => Some(ok(["U _OBJC_CLASS_$_XCUIApplication"])),
            _ => None,
        };
        match reply {
            Some(reply) => Box::pin(async move { Ok(Some(reply)) }),
            None => answer(cmd),
        }
    });
    // The installer's three attempts at the test bundle.
    h.bridge.failing_sends.store(2, Ordering::SeqCst);
    let device = h.device_with_udid("0000-0000");
    v(device.setup(&h.context).await)?;
    let before = h.worker.requests.lock().unwrap().len();
    let _ = device
        .prepare(h.options.dependencies.configuration.clone(), h.context.clone())
        .await;
    let requests = h.worker.requests.lock().unwrap()[before..].to_vec();
    let sent: Vec<String> = requests.iter().map(|r| r.command.join(" ")).collect();
    assert!(
        requests.iter().all(|r| r.timeout.to_millis() == Some(SHELL)),
        "every command is under `shell`: {sent:#?}"
    );
    let mut configuration = (*h.options.dependencies.configuration).clone();
    configuration.device_initialization_timeout_millis = 0;
    let budget = device.preparation_time_left_millis(&configuration).ok_or("a budget")?;
    Ok((requests.len() as i64, budget, sent))
}

fn budget_of(commands: i64) -> i64 {
    commands * SHELL + 2_000
}

async fn longest_path_is_counted(test_type: Option<TestType>, permissions: usize, expected: i64) -> R {
    let (sent, budget, commands) = variant(test_type, permissions, false).await?;
    assert_eq!(sent, expected, "{commands:#?}");
    assert_eq!(budget, budget_of(sent), "{commands:#?}");
    Ok(())
}

/// The defaults, the test type left to the probe, which finds XCUITest: 25 commands, 752 s.
#[tokio::test]
async fn a_preparation_with_the_test_type_left_to_the_probe_sends_what_its_budget_counts_d87() -> R {
    longest_path_is_counted(None, 0, 25).await
}

/// XCUITest configured: 24 commands, among them the removal before each of the runner's two overwriting copies.
#[tokio::test]
async fn an_xcuitest_preparation_sends_what_its_budget_counts_d87() -> R {
    longest_path_is_counted(Some(TestType::Xcuitest), 0, 24).await
}

/// Hosted XCTest with one permission: 12 commands, the SIP check and two writes, 15 (452 s).
#[tokio::test]
async fn a_hosted_xctest_preparation_with_a_permission_sends_what_its_budget_counts_d87() -> R {
    longest_path_is_counted(Some(TestType::Xctest), 1, 15).await
}

/// Hosted XCTest with two permissions: 12, the SIP check and four writes, 17.
#[tokio::test]
async fn a_hosted_xctest_preparation_with_two_permissions_sends_what_its_budget_counts_d87() -> R {
    longest_path_is_counted(Some(TestType::Xctest), 2, 17).await
}

/// Logic tests end in the test root factory's refusal, and with code coverage (no `xctestrun`, so no static libraries)
/// XCUITest sends what it sends without: neither sends more than its budget counts.
#[tokio::test]
async fn logic_tests_and_code_coverage_send_no_more_than_their_budget_counts_d87() -> R {
    for (test_type, coverage) in [(Some(TestType::LogicTest), false), (Some(TestType::Xcuitest), true)] {
        let (sent, budget, commands) = variant(test_type, 0, coverage).await?;
        assert!(budget_of(sent) <= budget, "{test_type:?} {coverage}: {commands:#?}");
    }
    Ok(())
}
