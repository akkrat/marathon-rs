//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/StartupPhasesTest.kt (generated xctestrun rows).
//! Factory-only oracle witnesses complement the four reduced preparation harnesses approved in 4.5b.
mod support;
use base64::{Engine, engine::general_purpose::STANDARD};

use marathon_apple::{
    cmd::cancelled,
    generated_file,
    model::AppleTestBundle,
    xctestrun::test_root_factory::{FactoryError, TestRootFactory},
};
use marathon_config::{devices::Transport, vendor::*};
use marathon_core::{
    error::VendorError,
    vendor::{Device, VendorContext},
};
use serde_json::{Value, json};
use std::sync::Arc;

#[path = "support/test_root_device.rs"]
mod test_root_device;
use test_root_device::*;

async fn generate(device: &Fake, bundle: &AppleTestBundle, context: &VendorContext) -> Result<(), FactoryError> {
    let c = &device.input;
    let env = env(c);
    let xcresult = xcresult(c);
    TestRootFactory::new(device, &env, &xcresult, b(c, "skip"), Arc::new(support::Runtime))
        .generate(
            match s(c, "type", "") {
                "XCTEST" => TestType::Xctest,
                "LOGIC_TEST" => TestType::LogicTest,
                _ => TestType::Xcuitest,
            },
            bundle,
            b(c, "parser"),
            context,
        )
        .await
}
/// D76 (A9): the JVM pushes a hosted iOS xctest that sits in the app's `PlugIns` again after the app push already
/// carried it; the native factory does not. Exactly these recordings lose exactly that one shared push, from both the
/// ordered events and the sends; every other recording, including macOS apps whose xctest is under `Contents`, is
/// compared unchanged.
const D76_ROWS: [&str; 2] = ["a9-embedded", "baseline-ios"];

fn d76_without_the_carried_push(row: &Value) -> Value {
    let c = &row["input"];
    let id = s(c, "id", "");
    let mut row = row.clone();
    if !D76_ROWS.contains(&id) {
        return row;
    }
    let (app, test) = (s(c, "app", ""), s(c, "test", ""));
    let name = test.rsplit('/').next().unwrap();
    assert_eq!(
        test,
        format!("{app}/PlugIns/{name}"),
        "{id}: D76 needs an xctest in the app's PlugIns"
    );
    let carried =
        json!({"kind":"shared","src":test,"dst":format!("/tmp/marathon/shared/appUnderTest.app/PlugIns/{name}")});
    for field in ["events", "pushes"] {
        let list = row[field].as_array_mut().unwrap();
        let before = list.len();
        list.retain(|entry| *entry != carried);
        assert_eq!(before - list.len(), 1, "{id}: D76 expects one carried push in {field}");
    }
    row
}

#[test]
fn d76_allowance_drops_only_the_carried_push_of_the_listed_rows() {
    let row = |id: &str, test: &str| {
        let push =
            json!({"kind":"shared","src":test,"dst":"/tmp/marathon/shared/appUnderTest.app/PlugIns/UITests.xctest"});
        let app = json!({"kind":"shared","src":"CloudApp.app","dst":"/tmp/marathon/shared/appUnderTest.app"});
        json!({"input":{"id":id,"app":"CloudApp.app","test":test},"events":[app,push],"pushes":[app,push]})
    };
    let kept = d76_without_the_carried_push(&row("a9-embedded", "CloudApp.app/PlugIns/UITests.xctest"));
    assert_eq!(kept["pushes"].as_array().unwrap().len(), 1);
    assert_eq!(kept["events"][0]["src"], "CloudApp.app");
    // An unlisted row keeps the push, so a native run that pushed it there would still have to match.
    let other = row(
        "IPHONESIMULATOR-XCTEST-False-False-False",
        "CloudApp.app/PlugIns/UITests.xctest",
    );
    assert_eq!(d76_without_the_carried_push(&other), other);
}

#[test]
#[should_panic(expected = "D76 expects one carried push")]
fn d76_allowance_refuses_a_listed_row_without_the_push() {
    let row = json!({"input":{"id":"a9-embedded","app":"CloudApp.app","test":"CloudApp.app/PlugIns/UITests.xctest"},
        "events":[],"pushes":[]});
    d76_without_the_carried_push(&row);
}

#[test]
#[should_panic(expected = "D76 needs an xctest in the app's PlugIns")]
fn d76_allowance_refuses_an_xctest_outside_the_app() {
    let row = json!({"input":{"id":"baseline-ios","app":"CloudApp.app","test":"UITests-Runner.app/PlugIns/UITests.xctest"},
        "events":[],"pushes":[]});
    d76_without_the_carried_push(&row);
}

#[tokio::test]
async fn factory_bytes_transcripts_errors_and_cleanup_match_jvm() -> TestResult {
    for row in rows() {
        let c = &row["input"];
        let id = s(c, "id", "");
        let temp = tempfile::tempdir()?;
        prepare(temp.path(), c)?;
        let device = Fake::new(c.clone(), temp.path());
        let bundle = bundle(c, temp.path(), device.logging.clone()).await?;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            generate(&device, &bundle, &support::context()),
        )
        .await?;
        let error = match outcome {
            Ok(()) => Value::Null,
            Err(error) => {
                let reason = match &error {
                    FactoryError::DeviceFailure { reason, .. } => Some(reason.name()),
                    _ => None,
                };
                let detail = error.failure();
                match &detail.kind {
                    marathon_error::FailureKind::DeviceSetup => {
                        assert!(matches!(error, FactoryError::Vendor(VendorError::DeviceSetup(_))))
                    }
                    marathon_error::FailureKind::Transfer => {
                        assert!(matches!(error, FactoryError::Vendor(VendorError::Transfer(_))))
                    }
                    marathon_error::FailureKind::IncompatibleDevice => assert!(matches!(
                        error,
                        FactoryError::Vendor(VendorError::IncompatibleDevice(_))
                    )),
                    marathon_error::FailureKind::Apple(marathon_error::AppleKind::DeviceFailure) => {
                        assert!(matches!(error, FactoryError::DeviceFailure { .. }))
                    }
                    _ => (),
                }

                // Only host bundle paths are typed-normalized, never emitted XML or worker argv.
                json!({"class":detail.kind.identifier().into_owned(),"message":detail.message.as_ref().map(|s|s.to_owned().replace(temp.path().to_str().unwrap(),"<bundle>")),"reason":reason})
            }
        };
        let mut expected_error = row["error"].clone();
        if !expected_error.is_null() {
            marathon_test_support::recorded_errors::error_object(&mut expected_error);
        }
        assert_eq!(error, expected_error, "{id}: error");
        let row = d76_without_the_carried_push(&row);
        assert_eq!(
            *device.recorder.events.lock().unwrap(),
            row["events"].as_array().unwrap().clone(),
            "{id}: ordered events"
        );
        assert_eq!(
            *device.recorder.requests.lock().unwrap(),
            row["commands"].as_array().unwrap().clone(),
            "{id}: commands"
        );
        assert_eq!(
            *device.pushes.lock().unwrap(),
            row["pushes"].as_array().unwrap().clone(),
            "{id}: pushes"
        );
        assert_eq!(json!(*device.xml.lock().unwrap()), row["xml"], "{id}: bytes");
        let paths = device.staged.lock().unwrap().clone();
        let mut cleaned = true;
        for path in paths {
            if tokio::fs::metadata(&path).await.is_ok() {
                cleaned = false;
                tokio::fs::remove_dir_all(path).await?;
            }
        }
        assert_eq!(cleaned, row["cleaned"], "{id}: cleanup");
        v(device.dispose().await)?;
    }
    Ok(())
}
async fn transfer_case(ssh: bool, debug: bool, fail: bool) -> TestResult {
    let mut c = input();
    c["reuse"] = true.into();
    c["skip"] = true.into();
    if fail {
        c["mode"] = "write-false".into();
    }
    let temp = tempfile::tempdir()?;
    prepare(temp.path(), &c)?;
    let mut device = Fake::new(c.clone(), temp.path());
    device.policy = true;
    device.debug = debug;
    if ssh {
        device.transport = Transport::Ssh {
            addr: "worker".into(),
            port: 22,
            authentication: None,
            check_reachability: false,
        };
    }
    let bundle = bundle(&c, temp.path(), device.logging.clone()).await?;
    v(tokio::time::timeout(
        std::time::Duration::from_secs(60),
        generate(&device, &bundle, &support::context()),
    )
    .await?)?;
    let writes = device.written.lock().unwrap().clone();
    let bridge = device.bridge.lock().unwrap().clone();
    assert_eq!(writes.len(), usize::from(ssh && !debug));
    assert_eq!(bridge.len(), usize::from(!ssh || debug || fail));
    for (dst, bytes) in writes.iter().chain(bridge.iter()) {
        assert_eq!(dst, "/tmp/marathon/a/marathon.xctestrun");
        assert_eq!(Some(STANDARD.encode(bytes)), *device.xml.lock().unwrap());
        let text = std::str::from_utf8(bytes)?;
        assert!(text.contains("<key>TestConfigurations</key>"));
        assert!(text.contains("/tmp/marathon/shared/appUnderTest.app"));
    }
    for path in device.staged.lock().unwrap().iter() {
        assert!(!path.exists());
    }
    drop(writes);
    drop(bridge);
    v(device.dispose().await)?;
    Ok(())
}
#[tokio::test]
async fn the_xctestrun_of_a_simulator_on_an_ssh_worker_is_written_through_its_connection() -> TestResult {
    transfer_case(true, false, false).await
}
#[tokio::test]
async fn the_xctestrun_is_pushed_through_the_file_bridge_if_writing_it_through_the_connection_fails() -> TestResult {
    transfer_case(true, false, true).await
}
#[tokio::test]
async fn with_ssh_debug_logging_the_xctestrun_is_pushed_through_the_file_bridge() -> TestResult {
    transfer_case(true, true, false).await
}
#[tokio::test]
async fn the_xctestrun_of_a_local_simulator_is_still_pushed_through_the_file_bridge() -> TestResult {
    transfer_case(false, false, false).await
}

#[tokio::test]
async fn real_bundle_factory_generation_matches_frozen_smoke_bytes() -> TestResult {
    for (id, scenario, index) in [
        ("baseline-ios", "ios-smoke", 0),
        ("baseline-macos-True", "macos-smoke", 0),
        ("baseline-macos-False", "macos-smoke", 1),
    ] {
        let row = rows().find(|r| r["input"]["id"] == id).unwrap();
        let c = &row["input"];
        let temp = tempfile::tempdir()?;
        prepare(temp.path(), c)?;
        let device = Fake::new(c.clone(), temp.path());
        let bundle = bundle(c, temp.path(), device.logging.clone()).await?;
        v(generate(&device, &bundle, &support::context()).await)?;
        let recorded: Value = serde_json::from_slice(&std::fs::read(
            fixtures()
                .join("fake-worker/apple")
                .join(scenario)
                .join("expected/raw/pushed-xctestrun.json"),
        )?)?;
        assert_eq!(
            device.xml.lock().unwrap().as_deref(),
            recorded[index]["base64"].as_str(),
            "{id}"
        );
        v(device.dispose().await)?;
    }
    Ok(())
}

#[tokio::test]
async fn generated_file_policy_boundaries_and_cancellation() -> TestResult {
    for (size, path, mode, writes, bridges) in [
        (32768, "/tmp/valid", "", 1, 0),
        (32769, "/tmp/valid", "", 0, 1),
        (1, "/tmp/unsafe path", "", 0, 1),
        (1, "/tmp/valid", "write-cancel", 0, 0),
    ] {
        let mut c = input();
        c["mode"] = mode.into();
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("input");
        tokio::fs::write(&file, vec![b'x'; size]).await?;
        let mut device = Fake::new(c, temp.path());
        device.transport = Transport::Ssh {
            addr: "worker".into(),
            port: 22,
            authentication: None,
            check_reachability: false,
        };
        let result = generated_file::push_generated_file(
            &device,
            &file,
            path,
            false,
            support::duration(31_000),
            &device.logging,
            &support::context(),
        )
        .await;
        if mode == "write-cancel" {
            assert!(matches!(result, Err(VendorError::Cancelled(_))));
        } else {
            assert!(v(result)?);
        }
        assert_eq!(device.written.lock().unwrap().len(), writes);
        assert_eq!(device.bridge.lock().unwrap().len(), bridges);
        v(device.dispose().await)?;
    }
    Ok(())
}
#[tokio::test]
async fn cancellation_waits_for_send_then_cleans_the_staged_file() -> TestResult {
    let mut c = input();
    c["mode"] = "await-cancel".into();
    let temp = tempfile::tempdir()?;
    prepare(temp.path(), &c)?;
    let device = Arc::new(Fake::new(c.clone(), temp.path()));
    let bundle = bundle(&c, temp.path(), device.logging.clone()).await?;
    let signal = Arc::new(marathon_apple::cmd::SessionScope::default());
    let sent = device.clone();
    let generation =
        marathon_core::executor::bridge::call_vendor(&support::Runtime, signal.clone(), move |cancellation| {
            Box::pin(async move {
                let mut context = support::context();
                context.cancellation = cancellation;
                generate(&sent, &bundle, &context)
                    .await
                    .map_err(FactoryError::into_vendor)
            })
        });
    let cancellation = async {
        loop {
            if !device.staged.lock().unwrap().is_empty() {
                signal.cancel(cancelled("request"));
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(generation, cancellation)
    })
    .await?;
    assert!(matches!(result, Err(VendorError::Cancelled(_))));
    for path in device.staged.lock().unwrap().iter() {
        assert!(!path.exists());
    }
    v(device.dispose().await)?;
    Ok(())
}
#[tokio::test]
async fn lone_surrogate_environment_values_stage_as_replacement_characters() -> TestResult {
    for field in ["appEnvs", "testEnvs"] {
        let c = input();
        let temp = tempfile::tempdir()?;
        prepare(temp.path(), &c)?;
        let device = Fake::new(c.clone(), temp.path());
        let bundle = bundle(&c, temp.path(), device.logging.clone()).await?;
        let mut env = env(&c);
        if field == "appEnvs" {
            env.app_envs.push(("K".into(), String::from_utf16_lossy(&[0xd800])));
        } else {
            env.test_envs.push(("K".into(), String::from_utf16_lossy(&[0xdfff])));
        }
        let xcresult = xcresult(&c);
        v(
            TestRootFactory::new(&device, &env, &xcresult, false, Arc::new(support::Runtime))
                .generate(TestType::Xcuitest, &bundle, false, &support::context())
                .await,
        )?;
        let encoded = device.xml.lock().unwrap().clone().ok_or("missing staged plist")?;
        let bytes = STANDARD.decode(encoded)?;
        let xml = std::str::from_utf8(&bytes)?;
        assert!(xml.contains("<key>K</key>"));
        assert!(xml.contains("<string>�</string>"));
        v(device.dispose().await)?;
    }
    Ok(())
}

/// D76 witness: which hosted xctests are pushed after the app. `link` replaces the xctest inside the app by a symlink to
/// a copy outside it, which the app push carries only as a link.
async fn hosted_xctest_pushes(id: &str, link: bool) -> TestResult<Vec<Value>> {
    let row = rows().find(|r| r["input"]["id"] == id).unwrap();
    let c = &row["input"];
    let temp = tempfile::tempdir()?;
    prepare(temp.path(), c)?;
    if link {
        let inside = temp.path().join(s(c, "test", ""));
        let outside = temp.path().join("elsewhere");
        std::fs::create_dir(&outside)?;
        let target = outside.join(inside.file_name().unwrap());
        std::fs::rename(&inside, &target)?;
        std::os::unix::fs::symlink(&target, &inside)?;
    }
    let device = Fake::new(c.clone(), temp.path());
    let bundle = bundle(c, temp.path(), device.logging.clone()).await?;
    v(generate(&device, &bundle, &support::context()).await)?;
    let pushes = device.pushes.lock().unwrap().clone();
    v(device.dispose().await)?;
    Ok(pushes
        .into_iter()
        .filter(|p| p["kind"] == "shared" && p["src"].as_str().is_some_and(|src| src.ends_with(".xctest")))
        .collect())
}

#[tokio::test]
async fn a_hosted_xctest_inside_the_app_is_not_pushed_again_d76() -> TestResult {
    assert_eq!(hosted_xctest_pushes("a9-embedded", false).await?, Vec::<Value>::new());
    assert_eq!(hosted_xctest_pushes("baseline-ios", false).await?, Vec::<Value>::new());
    Ok(())
}

#[tokio::test]
async fn hosted_xctests_the_app_push_does_not_carry_are_still_pushed_d76() -> TestResult {
    for (id, link, src, name) in [
        (
            "IPHONESIMULATOR-XCTEST-False-False-False",
            false,
            "UITests-Runner.app/PlugIns/UITests.xctest",
            "UITests",
        ),
        ("a9-embedded", true, "CloudApp.app/PlugIns/UITests.xctest", "UITests"),
        (
            "baseline-macos-False",
            false,
            "AppleWorld.app/Contents/PlugIns/AppleWorldTests.xctest",
            "AppleWorldTests",
        ),
        (
            "MACOS-XCTEST-False-False-False",
            false,
            "UITests-Runner.app/Contents/PlugIns/UITests.xctest",
            "UITests",
        ),
    ] {
        assert_eq!(
            hosted_xctest_pushes(id, link).await?,
            vec![
                json!({"kind":"shared","src":src,"dst":format!("/tmp/marathon/shared/appUnderTest.app/PlugIns/{name}.xctest")})
            ],
            "{id}"
        );
    }
    Ok(())
}
