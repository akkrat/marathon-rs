//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/AppleBinaryEnvironment.kt
//! Additional wire tests (no Kotlin matrix rows): the wrappers over the merged SSH executor against the fake
//! worker. Each rule matches the exact exec string, so this pins the final quoting (A12: arguments that start and
//! end with a quote pass through) and that env is dropped over SSH (A2), plus how the reply is interpreted.
mod support;
use base64::{Engine, prelude::BASE64_STANDARD};
use marathon_logging::LogSettings;
use marathon_apple::{
    bin::{AppleBinaryEnvironment, Tools},
    cmd::CommandExecutor,
    cmd::remote::ssh::sshj::config::SshLogging,
    model::XcodeVersion,
};
use marathon_config::vendor::{AppleTimeoutParameters, Codec, Display, Mask, apple_timeout_configuration};
use std::sync::{Arc, Mutex};
use support::{
    Runtime, context,
    ssh::{chunk, connect, exact, fake, output, script},
};

const UDID: &str = "FE547FCD-A71D-4B09-BE3C-97C18FD0F667";

#[tokio::test(flavor = "multi_thread")]
async fn wrappers_send_these_exec_strings_over_ssh() {
    let swift =
        r#"sh -c 'nm -gUj "/b/x" | xargs -s 131072 xcrun swift-demangle | cut -d\  -f3 | grep -e "[\\.|_]"test'"#;
    let wires = [
        "xcrun xcodebuild -version",
        "sh -c 'ioreg -ad2 -c IOPlatformExpertDevice | plutil -extract IORegistryEntryChildren.0.IOPlatformUUID raw -'",
        "csrutil status",
        "xcrun simctl create 'My Phone' com.apple.CoreSimulator.SimDeviceType.iPhone-15",
        "xcrun simctl boot U",
        swift,
        "sqlite3 '/d b' 'select '\\''x'\\'''",
        "sh -c 'nohup xcrun simctl io U recordVideo --codec hevc --display internal --mask ignored --force /tmp/v.mp4 > /dev/null 2>&1 & echo $! > /tmp/p'",
        "xcrun xcresulttool merge --output-path '/r p' /a /b",
    ];
    let created = format!("No runtime specified, using 'iOS 18.0'\n{UDID}\n");
    let replies = [
        output(vec![chunk("Xcode 26.6\nBuild version 17F113\n")], 0),
        output(vec![chunk(" WORKER-UUID \n")], 0),
        output(vec![chunk("System Integrity Protection status: disabled.\n")], 0),
        output(vec![chunk(&created)], 0),
        output(vec![], 0),
        output(vec![chunk("  Tests.testA\n\nTests.testB\n")], 0),
        output(vec![], 0),
        output(vec![], 0),
        // A failed merge and an unmatched one (127) both throw, so the route check below is what pins the wire.
        output(vec![], 1),
    ];
    let rules = wires
        .iter()
        .zip(replies)
        .map(|(wire, reply)| exact(wire, reply))
        .collect();
    let fake = fake(script(rules), 1).await;
    let executor = Arc::new(connect(&fake).await);
    let tools = Tools {
        executor: executor.clone(),
        timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
        runtime: Arc::new(Runtime),
        logging: SshLogging::new(
            Arc::new(Mutex::new(LogSettings::default())),
            Arc::new(|_, _, _, _| {}),
        ),
    };
    let env = AppleBinaryEnvironment::with_tools(tools, 1_800_000, 300_000);
    let c = &context();
    let simctl = &env.xcrun.simctl;

    assert_eq!(
        env.xcrun.xcodebuild.get_version(c).await.unwrap(),
        XcodeVersion::new(26, 6, 0)
    );
    assert_eq!(env.ioreg.get_udid(c).await.unwrap(), "WORKER-UUID");
    // A18: the real single line still reads as enabled.
    assert!(env.csrutil.status(c).await.unwrap());
    assert_eq!(
        simctl
            .simulator
            .create("My Phone", "com.apple.CoreSimulator.SimDeviceType.iPhone-15", None, c)
            .await
            .unwrap()
            .as_deref(),
        Some(UDID)
    );
    // A2/A10: the env never reaches the worker.
    assert!(
        simctl
            .simulator
            .boot("U", &[("K".into(), "v".into())], c)
            .await
            .unwrap()
    );
    assert_eq!(
        env.nm.swift_tests("/b/x", c).await.unwrap(),
        vec!["Tests.testA", "Tests.testB"]
    );
    env.sqlite3.query("/d b", "select 'x'", true, c).await.unwrap();
    assert!(
        simctl
            .io
            .record_video(
                "U",
                "/tmp/v.mp4",
                Codec::Hevc,
                Display::Internal,
                Mask::Ignored,
                "/tmp/p",
                c
            )
            .await
            .unwrap()
            .is_some_and(|r| r.successful())
    );
    let merge = env
        .xcrun
        .xcresulttool
        .merge("/r p", &["/a".into(), "/b".into()], c)
        .await;
    assert!(merge.is_err(), "critical: a failed merge throws before returning false");

    let execs: Vec<String> = fake
        .events("exec")
        .await
        .iter()
        .map(|e| {
            let wire = BASE64_STANDARD.decode(e["detail"]["base64"].as_str().unwrap()).unwrap();
            String::from_utf8(wire).unwrap()
        })
        .collect();
    assert_eq!(execs, wires);
    let routes = fake.events("route").await;
    assert_eq!(routes.len(), wires.len(), "{routes:?}");
    assert!(routes.iter().all(|r| r["detail"]["warning"].is_null()), "{routes:?}");
    let requests = fake.events("request").await;
    // The fake records an env request only by rejecting it.
    assert!(requests.iter().all(|e| e["detail"]["kind"] != "env"), "{requests:?}");

    drop(env);
    executor.close().await.unwrap();
    let support::ssh::Fake { dir, worker, .. } = fake;
    worker.shutdown().await.unwrap();
    drop(dir);
}

/// D32: the exec string of the one command with the variable, as a worker receives it.
#[tokio::test(flavor = "multi_thread")]
async fn xcodebuild_runs_on_the_worker_with_the_unbuffered_prefix_d32() {
    let wire = "env NSUnbufferedIO=YES xcrun xcodebuild test-without-building -xctestrun /tmp/marathon/U/marathon.xctestrun -enableCodeCoverage NO -destination-timeout 30 -destination 'platform=iOS Simulator,id=U'";
    let fake = fake(script(vec![exact(wire, output(vec![chunk("done\n")], 0))]), 1).await;
    let executor = Arc::new(connect(&fake).await);
    let tools = Tools {
        executor: executor.clone(),
        timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
        runtime: Arc::new(Runtime),
        logging: SshLogging::new(
            Arc::new(Mutex::new(LogSettings::default())),
            Arc::new(|_, _, _, _| {}),
        ),
    };
    let env = AppleBinaryEnvironment::with_tools(tools, 1_800_000, 300_000);
    let c = &context();
    let request =
        marathon_apple::test::TestRequest::new("/tmp/marathon/U", "/tmp/marathon/U/marathon.xctestrun", false);
    let session = env
        .xcrun
        .xcodebuild
        .test_without_building("U", marathon_apple::model::Sdk::IphoneSimulator, &request, &[], c)
        .await
        .unwrap();
    assert_eq!(session.await_result(c).await.unwrap().stdout, ["done"]);
    session.close().await.unwrap();
    let events = fake.events("exec").await;
    assert_eq!(events.len(), 1);
    assert!(fake.events("warning").await.is_empty(), "the rule did not match");
}
