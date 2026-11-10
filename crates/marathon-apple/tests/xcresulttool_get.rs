//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/xcresulttool/XcresulttoolGetTest.kt

mod support;
use marathon_logging::LogSettings;

use jcompat::jtime::Duration;
use marathon_apple::{
    bin::{
        Tools,
        xcrun::xcresulttool::{ResultBundleFormat, Xcresulttool},
    },
    cmd::{CommandResult, remote::ssh::sshj::config::SshLogging, test_support::FakeCommandExecutor},
    model::XcodeVersion,
    xcresult::{ActionTestPlanRunSummaries, ActionsInvocationRecord},
};
use marathon_config::vendor::{AppleTimeoutParameters, apple_timeout_configuration};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use support::*;

const VERSIONS: [(Option<&str>, bool); 6] = [
    (None, false),
    (Some("15.4"), false),
    (Some("16.0"), true),
    (Some("16.4"), true),
    (Some("26.6"), true),
    (Some("27.0"), true),
];

fn json() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vendor-apple-base/fixtures/xctesttool/sample_1.json");
    std::fs::read_to_string(path).unwrap()
}
/// Kotlin `String.lines()`.
fn lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect()
}
fn xcresulttool(
    stdout: Vec<String>,
    stderr: Vec<String>,
    exit_code: Option<i32>,
) -> (Xcresulttool, Arc<FakeCommandExecutor>) {
    let executor = Arc::new(FakeCommandExecutor::new(
        Arc::new(Runtime),
        CommandResult {
            stdout,
            stderr,
            exit_code,
        },
        &mut LogSettings::default(),
    ));
    let mut parameters = AppleTimeoutParameters::empty();
    parameters.xcresulttool = Some(Duration::of_seconds(7 * 60));
    let tools = Tools {
        executor: executor.clone(),
        timeouts: Arc::new(apple_timeout_configuration(parameters)),
        runtime: Arc::new(Runtime),
        logging: SshLogging::new(Arc::new(Mutex::new(LogSettings::default())), Arc::new(|_, _, _, _| {})),
    };
    (Xcresulttool::new(tools), executor)
}
fn version(xcode: Option<&str>) -> Option<XcodeVersion> {
    xcode.map(|v| XcodeVersion::from(v).unwrap())
}
fn command(legacy: bool, tail: &[&str]) -> Vec<String> {
    let mut command = vec!["xcrun".to_owned(), "xcresulttool".into(), "get".into()];
    if legacy {
        command.push("--legacy".into());
    }
    command.extend(tail.iter().map(|s| s.to_string()));
    command
}

#[tokio::test]
async fn parses_complete_output() {
    for (xcode, legacy) in VERSIONS {
        let (tool, executor) = xcresulttool(lines(&json()), vec![], Some(0));
        let record = tool
            .get::<ActionsInvocationRecord>(
                "/tmp/result.xcresult",
                version(xcode),
                ResultBundleFormat::Json,
                None,
                None,
                &context(),
            )
            .await
            .unwrap();

        let record = record.expect("record");
        assert!(!record.issues.test_failure_summaries.is_empty(), "{xcode:?}");
        assert_eq!(*executor.timeout.lock().unwrap(), Some(Duration::of_seconds(7 * 60)));
        assert_eq!(
            *executor.commands.lock().unwrap(),
            vec![command(legacy, &["--format", "json", "--path", "/tmp/result.xcresult"])]
        );
    }
}

#[tokio::test]
async fn object_query_keeps_its_id() {
    for (xcode, legacy) in VERSIONS {
        let (tool, executor) = xcresulttool(vec![], vec![], Some(0));
        let summaries = tool
            .get::<ActionTestPlanRunSummaries>(
                "/tmp/result.xcresult",
                version(xcode),
                ResultBundleFormat::Json,
                Some("0~id"),
                None,
                &context(),
            )
            .await
            .unwrap();
        assert!(summaries.is_none());
        assert_eq!(
            *executor.commands.lock().unwrap(),
            vec![command(
                legacy,
                &["--format", "json", "--path", "/tmp/result.xcresult", "--id", "0~id"]
            )]
        );
    }
}

async fn get(tool: &Xcresulttool, xcode: Option<&str>) -> Option<ActionsInvocationRecord> {
    tool.get::<ActionsInvocationRecord>(
        "/tmp/result.xcresult",
        version(xcode),
        ResultBundleFormat::Json,
        None,
        None,
        &context(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn returns_null_on_truncated_output() {
    for (xcode, _) in VERSIONS {
        let json = json();
        // json.length / 2 in UTF-16 units; the fixture is ASCII.
        let truncated = &json[..json.len() / 2];
        let (tool, _) = xcresulttool(lines(truncated), vec![], Some(0));
        assert!(get(&tool, xcode).await.is_none(), "{xcode:?}");
    }
}

#[tokio::test]
async fn returns_null_on_timed_out_command() {
    for (xcode, _) in VERSIONS {
        let (tool, _) = xcresulttool(lines(&json()), vec![], None);
        assert!(get(&tool, xcode).await.is_none(), "{xcode:?}");
    }
}

/// What Xcode 16 and later answer to `xcresulttool get` without `--legacy`.
#[tokio::test]
async fn returns_null_on_rejected_command() {
    for (xcode, _) in VERSIONS {
        let stderr = vec![
            "Error: This command is deprecated and will be removed in a future release, --legacy flag is required to use it.".to_owned(),
            "Usage: xcresulttool get object [--legacy] --path <path> [--id <id>] [--version <version>] [--format <format>]".to_owned(),
        ];
        let (tool, _) = xcresulttool(vec![], stderr, Some(64));
        assert!(get(&tool, xcode).await.is_none(), "{xcode:?}");
    }
}
