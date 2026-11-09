//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/xcodebuild/XcodebuildTest.kt

mod support;
use marathon_logging::LogSettings;
use marathon_apple::{
    bin::{
        Tools,
        xcrun::xcodebuild::{Xcodebuild, simulator_test_args},
    },
    cmd::{CommandResult, remote::ssh::sshj::config::SshLogging, test_support::FakeCommandExecutor},
    model::{Sdk, XcodeVersion},
    test::TestRequest,
};
use marathon_config::vendor::{AppleTimeoutParameters, apple_timeout_configuration};
use std::sync::{Arc, Mutex};
use support::*;

fn xcodebuild(result: CommandResult) -> (Xcodebuild, Arc<FakeCommandExecutor>) {
    let executor = Arc::new(FakeCommandExecutor::new(
        Arc::new(Runtime),
        result,
        &mut LogSettings::default(),
    ));
    let tools = Tools {
        executor: executor.clone(),
        timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
        runtime: Arc::new(Runtime),
        logging: SshLogging::new(
            Arc::new(Mutex::new(LogSettings::default())),
            Arc::new(|_, _, _, _| {}),
        ),
    };
    // Configuration.Builder defaults: 30 min batch, 5 min output.
    (Xcodebuild::new(tools, 1_800_000, 300_000), executor)
}
fn map(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}
fn args(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn parses_the_version_that_xcodebuild_prints() {
    for (stdout, major, minor, patch) in [
        ("Xcode 15.4|Build version 15F31d", 15, 4, 0),
        ("Xcode 16.0|Build version 16A242d", 16, 0, 0),
        ("Xcode 26.6|Build version 17F113", 26, 6, 0),
        ("Xcode 26.0.1|Build version 17A400", 26, 0, 1),
        ("Xcode 27.0|Build version 27A266a", 27, 0, 0),
    ] {
        let (xcodebuild, executor) = xcodebuild(CommandResult {
            stdout: stdout.split('|').map(str::to_owned).collect(),
            stderr: vec![],
            exit_code: Some(0),
        });
        assert_eq!(
            xcodebuild.get_version(&context()).await.unwrap(),
            XcodeVersion::new(major, minor, patch)
        );
        assert_eq!(
            *executor.commands.lock().unwrap(),
            vec![args(&["xcrun", "xcodebuild", "-version"])]
        );
    }
}

#[test]
fn test_diagnostics_are_off_by_default_from_xcode_14_on() {
    for (xcode, off) in [
        ("13.0", false),
        ("13.4.1", false),
        ("14.0", true),
        ("15.4", true),
        ("16.4", true),
        ("26.6", true),
        ("27.0", true),
        ("28.0", true),
    ] {
        let user = map(&[("-test-timeouts-enabled", "YES")]);
        let args = simulator_test_args(&user, Some(XcodeVersion::from(xcode).unwrap()));
        // Kotlin compares maps; the order is asserted as well, it reaches argv.
        let expected = if off {
            map(&[
                ("-collect-test-diagnostics", "never"),
                ("-test-timeouts-enabled", "YES"),
            ])
        } else {
            user
        };
        assert_eq!(args, expected, "Xcode {xcode}");
    }
}

#[test]
fn an_unknown_xcode_version_adds_no_arguments() {
    assert_eq!(simulator_test_args(&[], None), vec![]);
}

#[test]
fn the_users_test_diagnostics_argument_wins() {
    for value in ["on-failure", "never"] {
        let user = map(&[("-test-timeouts-enabled", "YES"), ("-collect-test-diagnostics", value)]);
        assert_eq!(
            simulator_test_args(&user, Some(XcodeVersion::new(27, 0, 0))),
            map(&[("-collect-test-diagnostics", value), ("-test-timeouts-enabled", "YES")])
        );
    }
}

#[tokio::test]
async fn xcodebuild_runs_tests_with_the_arguments_of_the_workers_xcode() {
    for (xcode, off) in [("13.4.1", false), ("14.0", true), ("26.6", true)] {
        let (xcodebuild, executor) = xcodebuild(CommandResult {
            stdout: vec![],
            stderr: vec![],
            exit_code: Some(0),
        });
        let mut request = TestRequest::new("/tmp/marathon/udid", "/tmp/marathon/udid/marathon.xctestrun", false);
        request.xcresult = Some("/tmp/r.xcresult".into());
        let user = map(&[("-test-timeouts-enabled", "YES"), ("-destination", "ignored")]);

        xcodebuild
            .test_without_building(
                "udid",
                Sdk::IphoneSimulator,
                &request,
                &simulator_test_args(&user, Some(XcodeVersion::from(xcode).unwrap())),
                &context(),
            )
            .await
            .unwrap();

        let mut expected = args(&[
            "xcrun",
            "xcodebuild",
            "test-without-building",
            "-xctestrun",
            "/tmp/marathon/udid/marathon.xctestrun",
        ]);
        if off {
            expected.extend(args(&["-collect-test-diagnostics", "never"]));
        }
        expected.extend(args(&[
            "-test-timeouts-enabled",
            "YES",
            "-destination",
            "'platform=iOS Simulator,id=udid'",
            "-enableCodeCoverage",
            "NO",
            "-resultBundlePath",
            "/tmp/r.xcresult",
            "-destination-timeout",
            "30",
        ]));
        assert_eq!(*executor.commands.lock().unwrap(), vec![expected], "Xcode {xcode}");
    }
}

/// D32 happens in the SSH executor: the request a local executor gets keeps the plain command and the env map, so a local
/// process gets the variable once, from its environment.
#[tokio::test]
async fn the_request_keeps_the_variable_in_its_env_and_out_of_its_command_d32() {
    let (xcodebuild, executor) = xcodebuild(CommandResult {
        stdout: vec![],
        stderr: vec![],
        exit_code: Some(0),
    });
    let request = TestRequest::new("/tmp/marathon/udid", "/tmp/marathon/udid/marathon.xctestrun", false);
    xcodebuild
        .test_without_building("udid", Sdk::IphoneSimulator, &request, &[], &context())
        .await
        .unwrap();
    assert_eq!(
        executor.commands.lock().unwrap()[0][..2],
        args(&["xcrun", "xcodebuild"])
    );
    assert_eq!(
        executor.envs.lock().unwrap()[0]
            .get("NSUnbufferedIO")
            .map(String::as_str),
        Some("YES")
    );
}
