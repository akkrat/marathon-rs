//! Port of vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/MacosDevice.kt (grant oracle replay).
mod support;
use marathon_apple::{apple_device::AppleDevice, cmd::CommandResult};
use marathon_config::vendor::MacosPermission;
use marathon_core::{
    error::{Failure, VendorResult},
    vendor::Device,
};
use marathon_macos::installer::MacosApplicationInstaller;
use serde_json::{Value, json};
use std::sync::Arc;
use support::*;

fn normalize_epoch(text: &str) -> String {
    let mut result = text.to_owned();
    if let Some(end) = result.find(");")
        && let Some(start) = result[..end].rfind(',')
        && result[start + 1..end].parse::<i64>().is_ok()
    {
        result.replace_range(start + 1..end, "<epoch>");
    }
    result
}
fn error_chain(result: &VendorResult<()>) -> Value {
    let mut rows = vec![];
    let mut current = result.as_ref().err().map(|e| e.failure());
    while let Some(f) = current {
        rows.push(
            json!({"class":f.kind.identifier(),"message":f.message.as_ref().map(|s|normalize_epoch(&s.to_owned()))}),
        );
        current = f.cause.as_deref();
    }
    json!(rows)
}
fn rows() -> impl Iterator<Item = Value> {
    include_str!("../../../fixtures/oracle/macos/probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
}
#[tokio::test]
async fn permissions_match_jvm_command_bytes_order_and_failures() -> R {
    for mode in [
        "permissions",
        "missing-user",
        "os10",
        "unknown",
        "sip-full",
        "sip-fail",
        "sql-first",
        "sql-second",
    ] {
        let mut h = Harness::new("")?;
        Arc::make_mut(&mut h.options.vendor).permissions.grant = vec![(
            "com.example.App".into(),
            vec![MacosPermission::Accessibility, MacosPermission::DeveloperTool],
        )];
        let original = h.worker.fake.before.lock().unwrap().clone();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        *h.worker.fake.before.lock().unwrap() = Arc::new(move |cmd| {
            let original = original.clone();
            let count = count.clone();
            Box::pin(async move {
                use marathon_test_support::apple::fake_simulator_worker::ok;
                let fail = || CommandResult {
                    stdout: vec![],
                    stderr: vec![],
                    exit_code: Some(1),
                };
                if cmd == ["env"] {
                    return Ok(Some(ok([
                        " HOME = /Different/home ",
                        "USER=tester",
                        " A = first ",
                        " A = last ",
                        "bare",
                    ]
                    .into_iter()
                    .filter(|s| mode != "missing-user" || !s.starts_with("USER=")))));
                }
                if cmd.first().is_some_and(|s| s == "sw_vers") {
                    return Ok(Some(ok([match mode {
                        "os10" => "10.15",
                        "unknown" => "Unknown",
                        _ => "11.0",
                    }])));
                }
                if cmd == ["csrutil", "status"] && mode == "sip-full" {
                    return Ok(Some(ok(["System Integrity Protection status: disabled."])));
                }
                if cmd == ["csrutil", "status"] && mode == "sip-fail" {
                    return Ok(Some(fail()));
                }
                if cmd.first().is_some_and(|s| s == "sqlite3") {
                    let n = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if (mode == "sql-first" && n == 1) || (mode == "sql-second" && n == 2) {
                        return Ok(Some(fail()));
                    }
                }
                original(cmd).await
            })
        });
        let d = h.device();
        v(d.setup(&h.context).await)?;
        let outcome = MacosApplicationInstaller::grant_permissions(&d, &h.context)
            .await
            .map_err(|e| e.into_vendor());
        let row = rows().find(|r| r["case"] == mode).ok_or("oracle row")?;
        let mut expected = row["error"].clone();
        marathon_test_support::recorded_errors::error_chain(&mut expected);
        assert_eq!(error_chain(&outcome), expected, "{mode}");
        let requests=h.worker.requests.lock().unwrap().iter().map(|r| {assert!(r.workdir.is_none());json!({"argv":r.command.iter().map(|s|normalize_epoch(s)).collect::<Vec<_>>(),"timeout":r.timeout.to_string(),"idle":r.idle_timeout.to_string(),"env":r.env})}).collect::<Vec<_>>();
        assert_eq!(json!(requests), row["requests"], "{mode}");
        assert_eq!(d.info().model.to_owned(), row["facts"]["model"]);
        assert_eq!(d.abi().to_owned(), row["facts"]["abi"]);
        v(d.dispose().await)?;
        v(d.join_pending().await)?;
    }
    Ok(())
}
#[tokio::test]
async fn empty_grants_skip_sip_and_p4_rejects_malformed_clients() -> R {
    let h = Harness::new("")?;
    let d = h.device();
    v(d.setup(&h.context).await)?;
    v(MacosApplicationInstaller::grant_permissions(&d, &h.context).await)?;
    assert!(
        !h.worker
            .fake
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.first().is_some_and(|s| s == "csrutil"))
    );
    for client in ["com.example.\"broken", "com.example.\ncontrol"] {
        let e = d
            .grant(client, &[MacosPermission::Accessibility], &h.context)
            .await
            .err()
            .ok_or("P4 failure")?;
        assert_eq!(e.failure().kind.description(), "invalid argument".to_owned());
    }
    assert!(
        !h.worker
            .fake
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.first().is_some_and(|s| s == "sqlite3"))
    );
    v(d.dispose().await)?;
    Ok(())
}
#[tokio::test]
async fn direct_unsupported_operations_fail_with_the_kotlin_error() -> R {
    use marathon_core::screenshot::Screenshottable;
    let h = Harness::new("")?;
    let d = h.device();
    let expected = |f: &Failure| {
        assert_eq!(
            f.kind,
            marathon_error::FailureKind::Fatal(marathon_error::FatalKind::NotImplemented)
        );
        assert_eq!(
            f.message,
            Some("An operation is not implemented: Not yet implemented".into())
        );
    };
    expected(
        d.get_screenshot(runtime::duration(100), h.root.path(), &h.context)
            .await
            .err()
            .ok_or("screenshot")?
            .failure(),
    );
    expected(
        d.screenshot(runtime::duration(100), h.context.cancellation.clone())
            .await
            .err()
            .ok_or("screenshot")?
            .failure(),
    );
    expected(
        d.start_video_recording("video", &h.context)
            .await
            .err()
            .ok_or("video")?
            .failure(),
    );
    expected(d.stop_video_recording(&h.context).await.err().ok_or("stop")?.failure());
    expected(d.shutdown(&h.context).await.err().ok_or("shutdown")?.failure());
    expected(d.erase(&h.context).await.err().ok_or("erase")?.failure());
    assert_eq!(v(d.orientation())?, marathon_core::screenshot::Rotation::Rotation0);
    assert!(d.info().healthy);
    assert!(d.info().device_features.is_empty());
    assert!(v(d.install("anything", &h.context).await)?);
    assert!(h.worker.requests.lock().unwrap().is_empty());
    v(d.dispose().await)?;
    Ok(())
}

#[tokio::test]
async fn grant_reads_wall_once_per_client_and_truncates_negative_epoch_toward_zero() -> R {
    use marathon_core::{
        executor::{
            RuntimeClock,
            hooks::{RuntimeHooks, Tape},
        },
        time::ReplayHooks,
    };
    let h = Harness::new("")?;
    let d = h.device();
    v(d.setup(&h.context).await)?;
    let clock = RuntimeClock::virtual_clock();
    let mut tape = Tape::default();
    tape.reads.insert("macos.MacosDevice.grant".into(), vec![-1, -1999]);
    let hooks = RuntimeHooks::taped(tape, &clock);
    let mut context = h.context.clone();
    context.hooks = hooks.clone();
    for client in ["first", "second"] {
        v(d.grant(
            client,
            &[MacosPermission::Accessibility, MacosPermission::DeveloperTool],
            &context,
        )
        .await)?;
    }
    v(hooks.verify_consumed())?;
    let commands = h
        .worker
        .fake
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.first().is_some_and(|s| s == "sqlite3"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(commands.len(), 8);
    assert!(commands[..4].iter().all(|c| c[2].contains(",0);")));
    assert!(commands[4..].iter().all(|c| c[2].contains(",-1);")));
    v(d.dispose().await)?;
    Ok(())
}
