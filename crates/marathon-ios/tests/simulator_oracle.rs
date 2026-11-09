//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//! Actual-device JVM witnesses from tools/oracle/simulator-device.
mod simulator_support;
mod support;
use marathon_apple::{apple_device::AppleDevice, cmd::CommandResult, plist::PlistValue};
use marathon_core::vendor::Device;
use serde_json::{Value, json};
use simulator_support::*;
use std::sync::{Arc, atomic::Ordering};
fn rows() -> Vec<Value> {
    include_str!("../../../fixtures/oracle/simulator-device/events.jsonl")
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
#[tokio::test(start_paused = true)]
async fn setup_command_order_and_metadata_match_the_actual_jvm_device() -> TestResult {
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    let oracle = rows();
    let commands: Vec<Value> = oracle
        .iter()
        .filter(|r| r["kind"] == "command" && r["scenario"] == "setup:happy")
        .map(|r| r["argv"].clone())
        .take_while(|v| {
            v.as_array().unwrap().first().unwrap() != "xcrun"
                || v.as_array().unwrap().last().unwrap() != "SIMULATOR_VERSION_INFO"
        })
        .collect();
    let rust = h.worker.commands.lock().unwrap().clone();
    let prefix: Vec<Value> = rust[..rust.len() - 1].iter().map(|v| json!(v)).collect();
    assert_eq!(prefix, commands);
    let setup = oracle
        .iter()
        .find(|r| r["kind"] == "setup" && r["scenario"] == "happy")
        .unwrap();
    assert_eq!(d.root_path(), setup["root"]);
    assert_eq!(d.info().healthy, setup["healthy"].as_bool().unwrap());
    assert_eq!(d.info().operating_system.version.to_owned(), setup["os"]);
    h.worker.connected.store(false, Ordering::SeqCst);
    let disconnected = oracle.iter().find(|r| r["kind"] == "disconnectedHealth").unwrap();
    assert_eq!(d.info().healthy, disconnected["healthy"].as_bool().unwrap());
    assert_eq!(d.info().network_state, marathon_core::model::NetworkState::Connected);
    h.worker.connected.store(true, Ordering::SeqCst);
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn jvm_setup_fallback_env_and_display_witnesses_match_rust() -> TestResult {
    for scenario in [
        "host-home",
        "missing-home",
        "root-first-data",
        "missing-plist",
        "env-line",
        "failed-displays",
    ] {
        let h = Harness::new(|_| {})?;
        let plan = h.boot.clone();
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            let ok = |lines: Vec<String>| marathon_test_support::apple::fake_simulator_worker::ok(lines);
            let reply = if cmd == ["env"] && scenario == "host-home" {
                Some(ok(vec!["USER=worker".into()]))
            } else if cmd == ["env"] && scenario == "env-line" {
                Some(ok(vec![
                    "HOME=/Users/worker".into(),
                    " NO_EQUAL ".into(),
                    " KEY = value = rest ".into(),
                ]))
            } else if cmd.last().is_some_and(|s| s == "SIMULATOR_HOST_HOME") {
                Some(ok(vec!["/Users/fallback".into()]))
            } else if cmd == ["xcrun", "simctl", "getenv", "a", "HOME"] && scenario == "missing-home" {
                Some(failing())
            } else if cmd == ["xcrun", "simctl", "getenv", "a", "HOME"] && scenario == "root-first-data" {
                Some(ok(vec!["/data/workers/a/data".into()]))
            } else if cmd.first().is_some_and(|s| s == "cat") && scenario == "missing-plist" {
                Some(failing())
            } else if cmd.first().is_some_and(|s| s == "/usr/sbin/system_profiler") && scenario == "failed-displays" {
                Some(CommandResult {
                    stdout: vec!["spdisplays_metalfamily".into()],
                    stderr: vec![],
                    exit_code: Some(1),
                })
            } else {
                plan.answer(&cmd)
            };
            Box::pin(async move { Ok(reply) })
        });
        let d = h.device().await?;
        let oracle = rows();
        let expected = oracle
            .iter()
            .find(|r| r["kind"] == "setup" && r["scenario"] == scenario)
            .unwrap();
        assert_eq!(d.root_path(), expected["root"]);
        let facts = d.load_worker_facts(&h.services.context).await?;
        let env = Value::Object(
            facts
                .env
                .iter()
                .map(|(k, v)| (k.to_owned(), json!(v.to_owned())))
                .collect(),
        );
        assert_eq!(env, expected["env"]);
        assert_eq!(
            facts
                .features
                .iter()
                .map(|f| format!("{f:?}").to_uppercase())
                .collect::<Vec<_>>(),
            serde_json::from_value::<Vec<String>>(expected["features"].clone())?
        );
        if scenario == "host-home" {
            assert!(
                d.grant(
                    marathon_config::vendor::IosPermission::UserTracking,
                    "app",
                    &h.services.context
                )
                .await
                .unwrap()
            );
            let cmds = h.worker.commands.lock().unwrap().clone();
            let rust = cmds.iter().find(|c| c.first().is_some_and(|s| s == "sqlite3")).unwrap();
            let jvm = oracle
                .iter()
                .find(|r| r["kind"] == "command" && r["scenario"] == "setup:host-home" && r["argv"][0] == "sqlite3")
                .unwrap();
            assert_eq!(json!(rust), jvm["argv"]);
        }
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn actual_jvm_wrong_state_types_fail_and_p2_accepts_integral_storage() -> TestResult {
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    for xml in [
        "<integer>1</integer>",
        "<real>1.0</real>",
        "<string>1</string>",
        "<true/>",
        "",
    ] {
        let content = format!(
            "<plist><dict>{}</dict></plist>",
            if xml.is_empty() {
                String::new()
            } else {
                format!("<key>state</key>{xml}")
            }
        );
        *h.worker.before.lock().unwrap() = Arc::new(move |c| {
            let reply = if c.first().is_some_and(|s| s == "cat") {
                Some(marathon_test_support::apple::fake_simulator_worker::ok([
                    content.clone()
                ]))
            } else {
                None
            };
            Box::pin(async move { Ok(reply) })
        });
        let result = d.simulator_state(&h.services.context).await;
        let oracle = rows();
        let expected = oracle
            .iter()
            .find(|r| r["kind"] == "actualState" && r["xml"] == xml)
            .unwrap();
        if let Some(class) = expected["errorClass"].as_str() {
            assert_eq!(
                result.unwrap_err().failure().kind.clone(),
                marathon_test_support::recorded_errors::kind(class)
            );
        } else {
            assert_eq!(
                result.unwrap(),
                if xml.is_empty() {
                    marathon_ios::simulator::State::Unknown
                } else {
                    marathon_ios::simulator::State::Shutdown
                }
            );
        }
    }
    let p = marathon_apple::plist::worker_descriptor(
        b"<plist><dict><key>state</key><integer>2147483648</integer></dict></plist>",
    )
    .unwrap();
    assert_eq!(p.get_cloned("state"), Some(PlistValue::Integer(2147483648)));
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn boot_deadline_readiness_and_poll_counts_match_the_jvm_virtual_clock() -> TestResult {
    for ready in [500, 501] {
        let h = Harness::new(|v| v.timeout_configuration.boot_wait = support::duration(500))?;
        let d = h.device().await?;
        let start = h.clock.now();
        let clock = h.clock.clone();
        let plan = h.boot.clone();
        *h.worker.before.lock().unwrap() = Arc::new(move |c| {
            let reply = if c.get(2).is_some_and(|s| s == "spawn") {
                Some(marathon_test_support::apple::fake_simulator_worker::ok([
                    if clock.now() - start >= ready {
                        "A com.apple.springboard.services"
                    } else {
                        "D com.apple.springboard.services"
                    },
                ]))
            } else {
                plan.answer(&c)
            };
            Box::pin(async move { Ok(reply) })
        });
        let before = h.boot.reads.load(Ordering::SeqCst);
        let booted = d.boot(&h.services.context).await.unwrap();
        let oracle = rows();
        let row = oracle
            .iter()
            .find(|r| r["kind"] == "bootDeadline" && r["readyMillis"] == ready)
            .unwrap();
        assert_eq!(booted, row["booted"].as_bool().unwrap());
        assert_eq!(h.clock.now() - start, row["elapsedMillis"].as_i64().unwrap());
        assert_eq!(
            h.boot.reads.load(Ordering::SeqCst) - before,
            row["reads"].as_u64().unwrap() as usize
        );
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn ignored_screenshot_timeout_matches_actual_jvm_commands() -> TestResult {
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    let file = h.dir.path().join("capture.png");
    for millis in [-1, 999] {
        assert!(
            d.get_screenshot(support::duration(millis), &file, &h.services.context)
                .await
                .unwrap()
        );
    }
    let commands = h.worker.commands.lock().unwrap().clone();
    let actual: Vec<_> = commands
        .iter()
        .filter(|c| c.get(4).is_some_and(|s| s == "screenshot"))
        .collect();
    let oracle = rows();
    let expected: Vec<_> = oracle
        .iter()
        .filter(|r| r["kind"] == "command" && r["scenario"] == "setup:happy" && r["argv"][4] == "screenshot")
        .collect();
    assert_eq!(actual.len(), 2);
    assert_eq!(expected.len(), 2);
    for (a, b) in actual.iter().zip(expected) {
        assert_eq!(json!(a), b["argv"]);
    }
    h.finish(&[d]).await;
    Ok(())
}
