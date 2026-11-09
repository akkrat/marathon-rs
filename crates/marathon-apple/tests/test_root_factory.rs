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
                match detail.kind.legacy_class().to_owned().as_str() {
                    "com.malinskiy.marathon.exceptions.DeviceSetupException" => {
                        assert!(matches!(error, FactoryError::Vendor(VendorError::DeviceSetup(_))))
                    }
                    "com.malinskiy.marathon.exceptions.TransferException" => {
                        assert!(matches!(error, FactoryError::Vendor(VendorError::Transfer(_))))
                    }
                    "com.malinskiy.marathon.exceptions.IncompatibleDeviceException" => assert!(matches!(
                        error,
                        FactoryError::Vendor(VendorError::IncompatibleDevice(_))
                    )),
                    "com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException" => {
                        assert!(matches!(error, FactoryError::DeviceFailure { .. }))
                    }
                    _ => (),
                }

                // Only host bundle paths are typed-normalized, never emitted XML or worker argv.
                json!({"class":detail.kind.legacy_class().to_owned(),"message":detail.message.as_ref().map(|s|s.to_owned().replace(temp.path().to_str().unwrap(),"<bundle>")),"reason":reason})
            }
        };
        assert_eq!(error, row["error"], "{id}: error");
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
