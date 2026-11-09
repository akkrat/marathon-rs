//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/FakeSimulatorWorker.kt
mod support;
use marathon_apple::bin::xcrun::simctl::model::decode_list_devices_output;
use marathon_apple::cmd::{CommandExecutor, CommandRequest, CommandResult, CommandSession};
use marathon_core::error::VendorError;
use marathon_test_support::apple::fake_simulator_worker::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::{Runtime, context, duration};
fn worker() -> FakeSimulatorWorker {
    FakeSimulatorWorker::new(Arc::new(Runtime), &mut jcompat::logging::MarathonLogging::default())
}
async fn run(w: &FakeSimulatorWorker, args: &[&str]) -> CommandResult {
    let ctx = context();
    let s = w
        .execute(CommandRequest::new(args.iter().copied(), duration(1000)), ctx.clone())
        .await
        .unwrap_or_else(|e| panic!("{e:?}"));
    let r = s.await_result(&ctx).await.unwrap_or_else(|e| panic!("{e:?}"));
    assert!(!s.is_alive());
    assert!(
        s.base
            .stdout
            .receive()
            .await
            .unwrap_or_else(|e| panic!("{e:?}"))
            .is_none()
    );
    s.close().await.unwrap_or_else(|e| panic!("{e:?}"));
    r
}
#[tokio::test]
async fn built_in_answers_inventory_and_shared_connections() -> anyhow::Result<()> {
    let w = worker().with_host_id("worker");
    w.add(["b3", "b1", "b2"]);
    assert_eq!(w.host().id, "worker");
    assert_eq!(w.host().charset, "UTF-8");
    assert_eq!(run(&w, ENV).await, ok(["HOME=/Users/worker", "USER=worker"]));
    assert_eq!(
        run(&w, XCODEBUILD_VERSION).await,
        ok(["Xcode 16.4", "Build version 16F6"])
    );
    assert_eq!(run(&w, UNAME).await, ok(["arm64"]));
    *w.arch.lock().unwrap() = "x86_64".into();
    *w.xcodebuild_version.lock().unwrap() = vec!["Xcode 1.0".into()];
    assert_eq!(run(&w, UNAME).await, ok(["x86_64"]));
    assert_eq!(run(&w, XCODEBUILD_VERSION).await, ok(["Xcode 1.0"]));
    assert_eq!(
        run(&w, &[SYSTEM_PROFILER, "any"]).await,
        ok(["<string>spdisplays_metalfamily</string>"])
    );
    assert_eq!(
        run(&w, &["xcrun", "simctl", "getenv", "b1", "HOME"]).await,
        ok(["/Users/worker/Library/Developer/CoreSimulator/Devices/b1/data"])
    );
    for (key, value) in [
        ("SIMULATOR_MODEL_IDENTIFIER", "model-b1"),
        ("SIMULATOR_RUNTIME_VERSION", "18.6"),
    ] {
        assert_eq!(run(&w, &["xcrun", "simctl", "getenv", "b1", key]).await, ok([value]));
    }
    let unknown = run(&w, &["xcrun", "simctl", "getenv", "b1", "OTHER"]).await;
    assert_eq!(
        unknown,
        CommandResult {
            stdout: vec![],
            stderr: vec![],
            exit_code: Some(1)
        }
    );
    assert_eq!(
        run(&w, &["xcrun", "simctl", "getenv", "missing", "HOME"]).await,
        invalid("device")
    );
    assert_eq!(run(&w, &["other"]).await.stderr, ["unsupported command"]);
    assert_eq!(run(&w, &["other"]).await.exit_code, Some(1));
    w.simulators.lock().unwrap()["b2"]
        .available
        .store(false, Ordering::SeqCst);
    let other = worker().with_simulators(w.simulators.clone());
    let id = run(&w, &["xcrun", "simctl", "create", "name", IPHONE]).await.stdout[0].clone();
    assert_eq!(uuid::Uuid::parse_str(&id)?.to_string().to_uppercase(), id);
    assert_eq!(w.simulators.lock().unwrap()[&id].runtime, IOS);
    let custom = run(
        &w,
        &["xcrun", "simctl", "create", "name", "custom-type", "custom-runtime"],
    )
    .await
    .stdout[0]
        .clone();
    let listed = run(&other, LIST).await;
    let parsed = decode_list_devices_output(&listed.combined_stdout())?;
    assert_eq!(parsed.devicetypes.len(), 1);
    assert_eq!(parsed.runtimes.len(), 1);
    assert_eq!(parsed.devices.devices.len(), 5);
    let json: serde_json::Value = serde_json::from_str(&listed.stdout[0])?;
    assert_eq!(json["devices"]["custom-runtime"][0]["udid"], custom);
    let b2 = parsed.devices.devices.iter().find(|d| d.udid == "b2").unwrap();
    assert_eq!(b2.is_available, Some(false));
    assert_eq!(format!("{:?}", b2.state), "Shutdown");
    assert_eq!(
        run(&other, &["xcrun", "simctl", "delete", &id]).await,
        ok(Vec::<String>::new())
    );
    assert_eq!(run(&w, &["xcrun", "simctl", "delete", &id]).await, invalid(&id));
    assert_eq!(w.getenv("b1", "HOME"), 1);
    assert_eq!(other.lists(), 1);
    assert_eq!(w.worker_fact_commands(), 6);
    Ok(())
}
#[tokio::test]
async fn hooks_record_first_obey_precedence_and_swap_while_suspended() -> anyhow::Result<()> {
    let test = async {
        let w = Arc::new(worker());
        let before = Arc::new(AtomicUsize::new(0));
        let calls = before.clone();
        *w.before.lock().unwrap() = Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Some(ok(["before"]))) })
        });
        let weak = Arc::downgrade(&w);
        *w.session.lock().unwrap() = Arc::new(move |argv| {
            assert_eq!(weak.upgrade().unwrap().count(argv), 1);
            Ok(Some(CommandSession::finished(Arc::new(Runtime), ok(["session"]))))
        });
        assert_eq!(run(&w, ENV).await, ok(["session"]));
        assert_eq!(before.load(Ordering::SeqCst), 0);
        *w.session.lock().unwrap() = Arc::new(|_| Ok(None));
        assert_eq!(run(&w, ENV).await, ok(["before"]));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let e = entered.clone();
        let r = release.clone();
        let weak = Arc::downgrade(&w);
        *w.before.lock().unwrap() = Arc::new(move |argv| {
            let (e, r, weak) = (e.clone(), r.clone(), weak.clone());
            Box::pin(async move {
                assert_eq!(weak.upgrade().unwrap().count(&argv), 1);
                e.notify_one();
                r.notified().await;
                Ok(Some(ok(["old"])))
            })
        });
        let running = w.clone();
        let task = tokio::spawn(async move { run(&running, UNAME).await });
        entered.notified().await;
        *w.before.lock().unwrap() = Arc::new(|_| Box::pin(async { Ok(None) }));
        w.add(["during-await"]);
        assert_eq!(run(&w, ENV).await, ok(["HOME=/Users/worker", "USER=worker"]));
        release.notify_one();
        assert_eq!(task.await?, ok(["old"]));
        *w.before.lock().unwrap() =
            Arc::new(|_| Box::pin(async { Err(VendorError::Other(connection_exception("lost"))) }));
        assert!(
            matches!(w.execute(CommandRequest::new(["env"],duration(1000)),context()).await,Err(VendorError::Other(f)) if f==connection_exception("lost"))
        );
        assert!(
            w.safe_execute(CommandRequest::new(["env"], duration(1000)), context())
                .await
                .map_err(|e| anyhow::anyhow!("{e:?}"))?
                .is_none()
        );
        Ok(())
    };
    support::bounded(5, test).await
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_flag_and_blocking_hook_are_independent_of_execution() -> anyhow::Result<()> {
    let w = Arc::new(worker());
    w.connected.store(false, Ordering::SeqCst);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let weak = Arc::downgrade(&w);
    *w.on_connected_check.lock().unwrap() = Arc::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
        *weak.upgrade().unwrap().on_connected_check.lock().unwrap() = Arc::new(|| {});
    });
    assert!(!w.connected());
    assert!(!w.connected());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(run(&w, UNAME).await, ok(["arm64"]));
    w.close().await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let (entered, wait) = tokio::sync::oneshot::channel();
    let entered = std::sync::Mutex::new(Some(entered));
    let (release, blocked) = std::sync::mpsc::channel();
    let blocked = std::sync::Mutex::new(blocked);
    *w.on_connected_check.lock().unwrap() = Arc::new(move || {
        if let Some(e) = entered.lock().unwrap().take() {
            let _ = e.send(());
        }
        assert!(
            blocked
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(60))
                .is_ok()
        );
    });
    let check = w.clone();
    let task = tokio::task::spawn_blocking(move || check.connected());
    wait.await?;
    assert_eq!(run(&w, ENV).await, ok(["HOME=/Users/worker", "USER=worker"]));
    release.send(())?;
    assert!(!task.await?);
    for (f, class) in [
        (
            transport_exception("t"),
            "net.schmizz.sshj.transport.TransportException",
        ),
        (io_exception("i"), "java.io.IOException"),
        (
            cancellation_exception("c"),
            "java.util.concurrent.CancellationException",
        ),
    ] {
        assert_eq!(f.kind, marathon_test_support::recorded_errors::kind(class));
    }
    Ok(())
}

#[tokio::test]
async fn frozen_kotlin_fake_oracle_answers_match() -> anyhow::Result<()> {
    let w = worker();
    w.add(["b3", "b1", "b2"]);
    w.add_with(["custom"], "custom-type", "custom-runtime");
    w.simulators.lock().unwrap()["b2"]
        .available
        .store(false, Ordering::SeqCst);
    w.connected.store(false, Ordering::SeqCst);
    for line in include_str!("../../../fixtures/oracle/fake-simulator/probes.jsonl").lines() {
        let row: serde_json::Value = serde_json::from_str(line)?;
        let argv: Vec<String> = serde_json::from_value(row["command"].clone())?;
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut actual = run(&w, &refs).await;
        let mut expected: Vec<String> = serde_json::from_value(row["stdout"].clone())?;
        if refs == LIST {
            fn ordered(text: &str) -> anyhow::Result<serde_json::Value> {
                let mut value: serde_json::Value = serde_json::from_str(text)?;
                for devices in value["devices"].as_object_mut().unwrap().values_mut() {
                    devices
                        .as_array_mut()
                        .unwrap()
                        .sort_by_key(|d| d["udid"].as_str().unwrap().to_owned());
                }
                Ok(value)
            }
            assert_eq!(ordered(&actual.stdout[0])?, ordered(&expected[0])?);
            actual.stdout.clear();
            expected.clear();
        }
        assert_eq!(actual.stdout, expected, "{argv:?}");
        assert_eq!(serde_json::to_value(&actual.stderr)?, row["stderr"]);
        assert_eq!(serde_json::to_value(actual.exit_code)?, row["exit_code"]);
    }
    Ok(())
}
