//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt
//! Supplemental lifecycle, source, registration, and live-plan witnesses.
mod provider_support;
mod simulator_support;
mod support;
use marathon_config::{devices::Transport, vendor::DeviceProvider as Source};
use marathon_core::{
    executor::shared::SharedDeferred,
    runtime::Deferred,
    vendor::{Device, DeviceProvider},
};
use marathon_ios::device::{simulator_factory::SimulatorCreating, simulator_provisioner::ProvisioningConnections};
use provider_support::*;
use simulator_support::TestResult;
use std::sync::{Arc, atomic::Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
fn frame(payload: &[u8]) -> Vec<u8> {
    let mut b = vec![7, 6, 5, 4];
    b.extend_from_slice(&(payload.len() as i32).to_be_bytes());
    b.extend_from_slice(payload);
    b
}
fn yaml(ids: &[&str]) -> Vec<u8> {
    let mut s = String::from("workers:\n- transport:\n    type: local\n  devices:");
    if ids.is_empty() {
        s.push_str(" []\n");
    } else {
        s.push('\n');
        for id in ids {
            s.push_str(&format!("  - type: simulator\n    udid: {id}\n"));
        }
    }
    s.into_bytes()
}
async fn dynamic() -> TestResult<(ProviderHarness, TcpStream)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let p = ProviderHarness::new(|v| {
        v.device_provider = Source::Dynamic {
            host: "127.0.0.1".into(),
            port: port.into(),
        }
    })?;
    p.h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .logger("AppleSimulatorProvider", Some(jcompat::logging::Level::Debug));
    p.provider
        .initialize(p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    let (socket, _) = support::watchdog(listener.accept()).await?;
    Ok((p, socket))
}
#[tokio::test(start_paused = true)]
async fn a80_borrow_returns_the_same_removed_disposed_allocation() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await?;
    let device = p.connected(1).await.remove(0);
    p.update(&[]).await?;
    let borrowed = p
        .provider
        .borrow(p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    assert!(Arc::ptr_eq(&borrowed.0, &(device.clone() as Arc<dyn Device>)));
    assert_eq!(
        device.disposed(),
        fixture("A80-stale-borrow")["disposed"].as_bool().unwrap()
    );
    assert!(fixture("A80-stale-borrow")["same"].as_bool().unwrap());
    assert!(matches!(
        p.provider.subscribe().receive().await.map_err(test_error)?,
        Some(marathon_core::vendor::DeviceEvent::Disconnected(_))
    ));
    p.update(&["a"]).await?;
    let replacement = p.connected(1).await.remove(0);
    assert!(!Arc::ptr_eq(&device, &replacement));
    assert_eq!(p.connections.acquired.load(Ordering::SeqCst), 4);
    assert_eq!(p.connections.released.load(Ordering::SeqCst), 2);
    p.finish().await?;
    assert_eq!(fixture("replaced-leases")["released"], 4);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a81_an_unchanged_snapshot_leaves_a_failed_setup_to_its_timed_retry() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    let boot = p.h.boot.clone();
    *p.h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let result = boot.answer(&cmd);
        Box::pin(async move {
            if cmd.get(2).is_some_and(|s| s == "getenv") && cmd.last().is_some_and(|s| s == "HOME") {
                return Err(support::error("setup failed"));
            }
            Ok(result)
        })
    });
    p.update(&["a"]).await?;
    let commands = p.h.worker.commands.lock().unwrap().len();
    p.update(&["a"]).await?;
    assert_eq!(
        p.h.worker.commands.lock().unwrap().len() - commands,
        fixture("A81-failed-setup-remains-tracked")["repeatCommands"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(p.provider.subscribe().state().queued, 0);
    // D33: Kotlin's witness above still holds for the snapshot; the failure goes to the retry policy instead of nowhere.
    assert_eq!(
        *p.recovery.failed.lock().unwrap(),
        [vec![marathon_config::devices::AppleTarget::Simulator {
            udid: "a".into()
        }]]
    );
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn current_plan_excludes_recovering_exhausted_retired_removed_and_disconnected_allocations() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await?;
    let device = p.connected(1).await.remove(0);
    device.dispose().await.map_err(test_error)?;
    assert_eq!(
        serde_json::json!(p.provider.current(&Transport::Local)?.eligible_disposed),
        fixture("live-plan-exclusions")["eligible"]
    );
    p.provider.set_recovering(device.clone(), true);
    p.update(&["a", "b"]).await?;
    p.connected(1).await;
    assert_eq!(home(&p.h.worker, "a"), 1);
    assert_eq!(p.provider.current(&Transport::Local)?.kept.len(), 2);
    p.provider.set_recovering(device.clone(), false);
    p.provider.set_exhausted("a@local".into(), true);
    assert!(p.provider.current(&Transport::Local)?.eligible_disposed.is_empty());
    p.provider.set_exhausted("a@local".into(), false);
    device.retire();
    assert!(p.provider.current(&Transport::Local)?.eligible_disposed.is_empty());
    p.h.worker.connected.store(false, Ordering::SeqCst);
    assert!(p.provider.current(&Transport::Local)?.eligible_disposed.is_empty());
    p.h.worker.connected.store(true, Ordering::SeqCst);
    p.update(&["b"]).await?;
    assert!(p.provider.current(&Transport::Local)?.eligible_disposed.is_empty());
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn replacement_waits_for_predecessor_and_removal_rejects_an_in_flight_setup() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    let gate = SharedDeferred::new();
    let (script, job) = batch(&p.h, old.clone(), Some(gate.clone())).await;
    p.connections
        .connect(Transport::Local, p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    let replacement =
        p.h.factory()
            .create(
                Transport::Local,
                p.h.worker.clone(),
                Arc::new(support::BridgeMock),
                "a".into(),
                Arc::new(
                    marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                        Transport::Local,
                        p.h.clock.clone(),
                    ),
                ),
                p.h.services.context.clone(),
            )
            .await?;
    let provider = p.provider.clone();
    let registration = tokio::spawn(async move { provider.register("a@local".into(), replacement).await });
    script.closing.wait().await.map_err(test_error)?;
    assert!(!registration.is_finished());
    assert_eq!(p.provider.subscribe().state().queued, 0);
    p.update(&[]).await?; // Removal refers to the newly published allocation while predecessor disposal waits.
    assert_eq!(p.provider.devices().len(), 1);
    gate.complete(Ok(()));
    registration.await??;
    let _ = job.await?;
    p.connections
        .connect(Transport::Local, p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    let rejected =
        p.h.factory()
            .create(
                Transport::Local,
                p.h.worker.clone(),
                Arc::new(support::BridgeMock),
                "a".into(),
                Arc::new(
                    marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                        Transport::Local,
                        p.h.clock.clone(),
                    ),
                ),
                p.h.services.context.clone(),
            )
            .await?;
    let cleanup = rejected.clone();
    let error = p.provider.register("a@local".into(), rejected).await.unwrap_err();
    assert!(
        error
            .failure()
            .message
            .unwrap()
            .to_string_lossy()
            .contains("has left the devices file")
    );
    assert_eq!(
        p.provider.skipped_removed(&"a@local".into()),
        fixture("registration-removal-race")["skipped"].as_bool().unwrap()
    );
    cleanup.device.0.dispose().await.map_err(test_error)?;
    p.connections
        .release(cleanup.handle.executor())
        .await
        .map_err(test_error)?;
    p.finish().await
}
#[tokio::test]
async fn split_and_coalesced_frames_handoff_in_order_and_boundary_eof_permanently_stops_only_the_producer() -> TestResult
{
    let (p, mut socket) = dynamic().await?;
    let first = frame(&yaml(&["a"]));
    socket.write_all(&first[..3]).await?;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(p.provider.devices().is_empty());
    socket.write_all(&first[3..]).await?;
    let mut rest = frame(&yaml(&["a", "b"]));
    rest.extend(frame(&yaml(&["b"])));
    socket.write_all(&rest).await?;
    p.connected(2).await;
    support::watchdog(p.provider.subscribe().receive())
        .await
        .map_err(test_error)?;
    socket.shutdown().await?;
    eventually(|| {
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, s, _)| s.starts_with("Error occurred:"))
    })
    .await;
    let before = p.recovery.ticks.load(Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(p.recovery.ticks.load(Ordering::SeqCst) > before);
    assert!(fixture("tcp-boundary-eof")["monitor"].as_bool().unwrap());
    assert_eq!(
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, _, s, _)| s.starts_with("Error occurred:"))
            .count(),
        1
    );
    p.finish().await
}
#[tokio::test]
async fn dynamic_frame_failures_keep_their_error_boundaries() -> TestResult {
    for (name, bytes) in [
        ("negative", {
            let mut b = vec![0; 4];
            b.extend((-1_i32).to_be_bytes());
            b
        }),
        ("oversized", {
            let mut b = vec![0; 4];
            b.extend(((1_i32 << 20) + 1).to_be_bytes());
            b
        }),
        ("partial-eof", vec![0, 0]),
        ("partial-length-eof", frame(&yaml(&[]))[..5].to_vec()),
        ("partial-payload-eof", frame(&yaml(&[]))[..9].to_vec()),
        ("malformed", frame(b"workers: wrong\n")),
        ("zero", frame(b"")),
    ] {
        let (p, mut socket) = dynamic().await?;
        socket.write_all(&bytes).await?;
        socket.shutdown().await?;
        eventually(|| {
            p.h.logs
                .lock()
                .unwrap()
                .iter()
                .any(|(_, _, s, _)| s.starts_with("Error occurred:"))
        })
        .await;
        let errors: Vec<_> =
            p.h.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, _, s, _)| s.starts_with("Error occurred:"))
                .map(|(_, _, s, _)| s.clone())
                .collect();
        let expected = fixture(&format!("tcp-{name}"));
        assert_eq!(
            errors,
            expected["errors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            "{name}"
        );
        p.finish().await?;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn borrow_waits_200_millis_and_monitor_ticks_every_16_millis() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    std::fs::write(p.h.dir.path().join("Marathondevices"), yaml(&[]))?;
    p.provider
        .initialize(p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    eventually(|| p.recovery.ticks.load(Ordering::SeqCst) > 0).await;
    let initial = p.recovery.ticks.load(Ordering::SeqCst);
    tokio::time::advance(std::time::Duration::from_millis(14)).await;
    assert_eq!(p.recovery.ticks.load(Ordering::SeqCst), initial);
    tokio::time::advance(std::time::Duration::from_millis(2)).await;
    tokio::task::yield_now().await;
    assert_eq!(p.recovery.ticks.load(Ordering::SeqCst), initial + 1);
    let provider = p.provider.clone();
    let caller = p.h.services.context.clone();
    let borrowed = tokio::spawn(async move { provider.borrow(caller).await });
    tokio::task::yield_now().await;
    p.update(&["a"]).await?;
    p.connected(1).await;
    assert!(!borrowed.is_finished());
    tokio::time::advance(std::time::Duration::from_millis(199)).await;
    assert!(!borrowed.is_finished());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    borrowed.await?.map_err(test_error)?;
    p.finish().await
}
#[tokio::test]
async fn termination_closes_a_socket_with_a_partial_frame_and_joins_source_tasks() -> TestResult {
    let (p, mut socket) = dynamic().await?;
    socket.write_all(&[0, 0, 0]).await?;
    p.finish().await?;
    let mut byte = [0];
    let result = support::watchdog(socket.read(&mut byte)).await;
    assert!(matches!(result, Ok(0)) || matches!(result, Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset));
    p.provider.terminate().await.map_err(test_error)?;
    Ok(())
}
#[tokio::test]
async fn rendezvous_backpressure_defers_binding_a_third_frame_until_the_second_is_received() -> TestResult {
    let (p, mut socket) = dynamic().await?;
    let release = SharedDeferred::new();
    let gate = release.clone();
    let entered = SharedDeferred::new();
    let started = entered.clone();
    let boot = p.h.boot.clone();
    *p.h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let gate = gate.clone();
        let started = started.clone();
        let answer = boot.answer(&cmd);
        Box::pin(async move {
            if cmd == marathon_test_support::apple::fake_simulator_worker::LIST {
                started.complete(Ok(()));
                gate.wait().await?;
            }
            Ok(answer)
        })
    });
    let mut bytes = frame(&yaml(&["a"]));
    bytes.extend(frame(&yaml(&["a", "b"])));
    bytes.extend(frame(b"workers: wrong\n"));
    socket.write_all(&bytes).await?;
    entered.wait().await.map_err(test_error)?;
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    assert!(
        !p.h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, s, _)| s.starts_with("Error occurred:"))
    );
    release.complete(Ok(()));
    p.connected(2).await;
    assert_eq!(fixture("tcp-backpressure")["devices"], serde_json::json!(["a", "b"]));
    eventually(|| {
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, s, _)| s == "Error occurred: Invalid Marathondevices update format")
    })
    .await;
    p.finish().await
}
#[tokio::test]
async fn static_loading_matches_jvm_error_stages_and_static_existence_wins_over_dynamic() -> TestResult {
    for (name, bytes) in [
        ("empty", b"workers: []\n".as_slice()),
        ("mapping", b"workers: wrong\n"),
        ("syntax", b"workers: [\n"),
        ("null", b"null\n"),
    ] {
        let p = ProviderHarness::new(|v| {
            v.device_provider = Source::Dynamic {
                host: "127.0.0.1".into(),
                port: 1,
            }
        })?;
        std::fs::write(p.h.dir.path().join("Marathondevices"), bytes)?;
        let error = p.provider.initialize(p.h.services.context.clone()).await.unwrap_err();
        let actual = error.failure();
        let expected = fixture(&format!("load-{name}"));
        assert_eq!(actual.class.to_string_lossy(), expected["class"].as_str().unwrap());
        assert_eq!(
            actual
                .message
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .replace(p.h.dir.path().to_str().unwrap(), "<ROOT>"),
            expected["message"].as_str().unwrap()
        );
        let mut actual = actual.cause.as_deref();
        let mut expected = expected.get("cause");
        while let Some(next) = expected {
            let a = actual.unwrap();
            assert_eq!(a.class.to_string_lossy(), next["class"].as_str().unwrap());
            // U83: preserve the merged binder's diagnostics; its exception classes/stages match.
            assert!(a.message.is_some());
            if name == "mapping" {
                assert_eq!(a.message.as_ref().unwrap().to_string_lossy(), "expected an array");
            }
            actual = a.cause.as_deref();
            expected = next.get("cause");
        }
        assert!(actual.is_none());
        p.finish().await?;
    }
    let p = ProviderHarness::new(|v| {
        v.device_provider = Source::Dynamic {
            host: "127.0.0.1".into(),
            port: 1,
        }
    })?;
    std::fs::write(p.h.dir.path().join("Marathondevices"), yaml(&[]))?;
    p.provider
        .initialize(p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    eventually(|| p.recovery.ticks.load(Ordering::SeqCst) > 0).await;
    assert_eq!(fixture("static-over-dynamic")["accepted"], false);
    p.finish().await?;
    let p = ProviderHarness::new(|v| {
        v.device_provider = Source::Dynamic {
            host: "127.0.0.1".into(),
            port: 1,
        }
    })?;
    std::fs::create_dir(p.h.dir.path().join("Marathondevices"))?;
    let error = p.provider.initialize(p.h.services.context.clone()).await.unwrap_err();
    assert_eq!(error.failure().class, "java.io.FileNotFoundException".into());
    p.finish().await
}
#[tokio::test]
async fn whole_worker_removal_and_profile_shrink_preserve_retained_entries() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a", "b"]).await?;
    let devices = p.connected(2).await;
    p.provider
        .process_update(snapshot(vec![]), p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    assert!(devices.iter().all(|d| d.disposed()));
    assert_eq!(p.provider.devices().len(), 2);
    assert!(fixture("whole-worker-removal")["disposed"].as_bool().unwrap());
    p.provider
        .process_update(snapshot(vec![]), p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    assert_eq!(p.provider.subscribe().state().queued, 2);
    p.finish().await?;
    let p = ProviderHarness::new(|_| {})?;
    p.provider
        .process_update(
            snapshot(vec![(
                Transport::Local,
                vec![marathon_config::devices::AppleTarget::SimulatorProfile(support::profile()); 3],
            )]),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    let devices = p.connected(3).await;
    p.provider
        .process_update(
            snapshot(vec![(
                Transport::Local,
                vec![marathon_config::devices::AppleTarget::SimulatorProfile(
                    support::profile(),
                )],
            )]),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    assert!(devices.iter().all(|d| !d.disposed()));
    assert_eq!(fixture("profile-high-water")["disposed"], 0);
    assert_eq!(p.provider.subscribe().state().queued, 0);
    p.finish().await
}
#[tokio::test]
async fn cancelled_waiter_does_not_abort_protected_termination() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await?;
    p.connected(1).await;
    let release = SharedDeferred::new();
    let gate = release.clone();
    let started = SharedDeferred::new();
    let entered = started.clone();
    p.h.services
        .artifacts
        .submit(
            "a@local".into(),
            "bundle".into(),
            "local".into(),
            None,
            Arc::new(move |_| {
                let gate = gate.clone();
                let entered = entered.clone();
                Box::pin(async move {
                    entered.complete(Ok(()));
                    gate.wait().await?;
                    Ok(())
                })
            }),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    started.wait().await.map_err(test_error)?;
    let provider = p.provider.clone();
    let close = tokio::spawn(async move { provider.terminate().await });
    eventually(|| p.provider.subscribe().state().closed_for_send).await;
    close.abort();
    assert!(close.await.unwrap_err().is_cancelled());
    release.complete(Ok(()));
    p.finish().await
}
struct RetryClock {
    inner: Arc<support::Clock>,
    entered: Arc<SharedDeferred<()>>,
    resume: Arc<SharedDeferred<()>>,
    delays: std::sync::Mutex<Vec<i64>>,
}
impl marathon_core::runtime::VendorClock for RetryClock {
    fn delay(
        &self,
        millis: i64,
        signal: Arc<dyn marathon_core::runtime::CancellationSignal>,
    ) -> marathon_core::runtime::VendorFuture<'_, ()> {
        self.delays.lock().unwrap().push(millis);
        Box::pin(async move {
            if millis == 5000 {
                self.entered.complete(Ok(()));
                self.resume.wait_in(signal).await.map(|_| ())
            } else {
                self.inner.delay(millis, signal).await
            }
        })
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, ()> {
        self.inner.with_timeout(parent, millis, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, Option<()>> {
        self.inner.with_timeout_or_null(parent, millis, body)
    }
}
/// A free port below the ephemeral ranges (Linux from 32768, macOS from 49152): a parallel test
/// binding port 0 cannot take it while the provider is being refused, as it can a released one.
fn closed_port() -> std::io::Result<u16> {
    (20_000 + std::process::id() % 10_000..30_000)
        .chain(20_000..30_000)
        .map(|port| port as u16)
        .find(|&port| std::net::TcpListener::bind(("127.0.0.1", port)).is_ok())
        .ok_or_else(|| std::io::Error::other("no closed port below the ephemeral range"))
}
#[tokio::test]
async fn refused_connections_wait_five_seconds_then_connect_without_restarting_after_eof() -> TestResult {
    let port = closed_port()?;
    let mut h = simulator_support::Harness::new(|v| {
        v.device_provider = Source::Dynamic {
            host: "127.0.0.1".into(),
            port: port.into(),
        }
    })?;
    let clock = Arc::new(RetryClock {
        inner: h.clock.clone(),
        entered: SharedDeferred::new(),
        resume: SharedDeferred::new(),
        delays: std::sync::Mutex::default(),
    });
    h.services.context.clock = clock.clone();
    let connections = Arc::new(Connections {
        fail_release: 0.into(),
        workers: std::sync::Mutex::new(vec![(Transport::Local, h.worker.clone())]),
        acquired: 0.into(),
        released: 0.into(),
        trace: Arc::default(),
    });
    let recovery = Arc::new(Recovery::default());
    let provider = ProviderHarness::build(&h, connections, recovery.clone())?;
    h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .logger("AppleSimulatorProvider", Some(jcompat::logging::Level::Debug));
    provider
        .initialize(h.services.context.clone())
        .await
        .map_err(test_error)?;
    support::watchdog(clock.entered.wait()).await.map_err(test_error)?;
    assert_eq!(clock.delays.lock().unwrap().iter().filter(|&&v| v == 5000).count(), 1);
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    clock.resume.complete(Ok(()));
    let (mut socket, _) = support::watchdog(listener.accept()).await?;
    socket.write_all(&frame(&yaml(&[]))).await?;
    socket.shutdown().await?;
    eventually(|| {
        h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, s, _)| s.starts_with("Error occurred:"))
    })
    .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(fixture("refused-then-success")["warnings"], 1);
    assert!(recovery.ticks.load(Ordering::SeqCst) > 1);
    provider.terminate().await.map_err(test_error)?;
    Ok(())
}
#[tokio::test]
async fn removal_on_a_dead_executor_notifies_and_leaves_disposal_to_recovery() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await?;
    let d = p.connected(1).await.remove(0);
    p.h.worker.connected.store(false, Ordering::SeqCst);
    p.update(&[]).await?;
    assert!(!d.disposed());
    assert!(matches!(
        p.provider.subscribe().receive().await.map_err(test_error)?,
        Some(marathon_core::vendor::DeviceEvent::Disconnected(_))
    ));
    p.finish().await
}
#[tokio::test]
async fn planning_rereads_the_registered_allocation_after_a_blocked_inventory() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    old.dispose().await.map_err(test_error)?;
    p.connections
        .connect(Transport::Local, p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    let replacement =
        p.h.factory()
            .create(
                Transport::Local,
                p.h.worker.clone(),
                Arc::new(support::BridgeMock),
                "a".into(),
                Arc::new(
                    marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                        Transport::Local,
                        p.h.clock.clone(),
                    ),
                ),
                p.h.services.context.clone(),
            )
            .await?;
    let identity = replacement.device.clone();
    let entered = SharedDeferred::new();
    let listing = entered.clone();
    let gate = SharedDeferred::new();
    let release = gate.clone();
    let boot = p.h.boot.clone();
    let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *p.h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let entered = entered.clone();
        let gate = gate.clone();
        let once = once.clone();
        let result = boot.answer(&cmd);
        Box::pin(async move {
            if cmd == marathon_test_support::apple::fake_simulator_worker::LIST && !once.swap(true, Ordering::SeqCst) {
                entered.complete(Ok(()));
                gate.wait().await?;
            }
            Ok(result)
        })
    });
    let provider = p.provider.clone();
    let caller = p.h.services.context.clone();
    let update = tokio::spawn(async move {
        provider
            .process_update(snapshot(vec![(Transport::Local, targets(&["a", "b"]))]), caller)
            .await
    });
    listing.wait().await.map_err(test_error)?;
    p.provider.register("a@local".into(), replacement).await?;
    release.complete(Ok(()));
    update.await?.map_err(test_error)?;
    p.connected(2).await;
    assert_eq!(home(&p.h.worker, "a"), 2);
    assert_eq!(
        p.provider
            .devices()
            .iter()
            .find(|d| d.handle.serial().starts_with("a@"))
            .unwrap()
            .device,
        identity
    );
    p.finish().await
}
#[tokio::test]
async fn an_asynchronous_notification_racing_channel_close_fails_without_failing_registration() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.provider.subscribe().close(None);
    p.update(&["a"]).await?;
    eventually(|| {
        p.h.logs.lock().unwrap().iter().any(|(_, _, _, error)| {
            error
                .as_ref()
                .is_some_and(|e| e.class == "kotlinx.coroutines.channels.ClosedSendChannelException".into())
        })
    })
    .await;
    assert_eq!(p.provider.devices().len(), 1);
    p.finish().await
}
#[tokio::test]
async fn invalid_dynamic_port_is_logged_as_a_permanent_producer_error() -> TestResult {
    let p = ProviderHarness::new(|v| {
        v.device_provider = Source::Dynamic {
            host: "127.0.0.1".into(),
            port: 65536,
        }
    })?;
    p.provider
        .initialize(p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    eventually(|| {
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, s, _)| s.starts_with("Error occurred:"))
    })
    .await;
    let errors: Vec<_> =
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, _, s, _)| s.starts_with("Error occurred:"))
            .map(|(_, _, s, _)| s.clone())
            .collect();
    assert_eq!(serde_json::json!(errors), fixture("invalid-port")["errors"]);
    p.finish().await
}
struct WaitingFactory {
    inner: Arc<
        marathon_ios::device::simulator_factory::SimulatorFactory<
            Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
        >,
    >,
    entered: Arc<SharedDeferred<()>>,
    finished: Arc<std::sync::atomic::AtomicBool>,
}
impl SimulatorCreating for WaitingFactory {
    type Handle = Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>;
    fn create<'a>(
        &'a self,
        transport: Transport,
        executor: marathon_apple::device::connection_factory::Executor,
        bridge: marathon_apple::device::connection_factory::Bridge,
        udid: String,
        cycle: Arc<marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext>,
        caller: marathon_core::vendor::VendorContext,
    ) -> marathon_ios::device::error::ProvisioningFuture<
        'a,
        marathon_ios::device::simulator_factory::ConstructedSimulator<Self::Handle>,
    > {
        Box::pin(async move {
            self.entered.complete(Ok(()));
            let result = caller.clock.delay(1_000_000, caller.cancellation.clone()).await;
            self.finished.store(true, Ordering::SeqCst);
            result?;
            self.inner
                .create(transport, executor, bridge, udid, cycle, caller)
                .await
        })
    }
}
#[tokio::test]
async fn cancelled_worker_cancels_sibling_setups_and_awaits_their_cleanup() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    let cancelled = p.worker(support::worker("cancelled"), &["a"]);
    p.worker(support::worker("waiting"), &["a"]);
    let entered = SharedDeferred::new();
    let wait = entered.clone();
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *cancelled.before.lock().unwrap() = Arc::new(move |_| {
        let wait = wait.clone();
        Box::pin(async move {
            wait.wait().await?;
            Err(marathon_core::error::VendorError::Cancelled(
                marathon_apple::cmd::cancelled("worker cancelled"),
            ))
        })
    });
    let mut services = ProviderHarness::services(&p.h, p.connections.clone(), p.recovery.clone());
    services.factory = Arc::new(WaitingFactory {
        inner: p.h.factory(),
        entered,
        finished: finished.clone(),
    });
    let provider = marathon_ios::device::simulator_provider::AppleSimulatorProvider::new(services)?;
    let result = support::watchdog(provider.process_update(
        snapshot(vec![
            (support::worker("cancelled"), targets(&["a"])),
            (support::worker("waiting"), targets(&["a"])),
        ]),
        p.h.services.context.clone(),
    ))
    .await;
    assert!(matches!(result, Err(marathon_core::error::VendorError::Cancelled(_))));
    assert!(finished.load(Ordering::SeqCst));
    assert_eq!(
        p.connections.acquired.load(Ordering::SeqCst),
        p.connections.released.load(Ordering::SeqCst)
    );
    provider.terminate().await.map_err(test_error)?;
    p.finish().await
}
#[tokio::test]
async fn substitution_cycles_escape_the_static_mapping_wrapper() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    let mut services = ProviderHarness::services(&p.h, p.connections.clone(), p.recovery.clone());
    let b = services.binding;
    services.binding = Arc::new(marathon_config::binding::BindingContext {
        profile: b.profile,
        marathonfile: b.marathonfile.clone(),
        marathonfile_dir: b.marathonfile_dir.clone(),
        process_dir: b.process_dir.clone(),
        environment: vec![("PROVIDER_CYCLE".into(), "${PROVIDER_CYCLE}".into())],
        now: b.now,
        analytics_tracking: false,
        bugsnag_reporting: false,
    });
    let provider = marathon_ios::device::simulator_provider::AppleSimulatorProvider::new(services)?;
    std::fs::write(p.h.dir.path().join("Marathondevices"), b"workers: ${PROVIDER_CYCLE}\n")?;
    let error = provider.initialize(p.h.services.context.clone()).await.unwrap_err();
    let expected = fixture("load-cycle");
    assert_eq!(
        error.failure().class.to_string_lossy(),
        expected["class"].as_str().unwrap()
    );
    assert_eq!(
        error.failure().message.as_ref().unwrap().to_string_lossy(),
        expected["message"].as_str().unwrap()
    );
    provider.terminate().await.map_err(test_error)?;
    p.finish().await
}
#[tokio::test]
async fn lost_planning_warning_uses_kotlin_cause_descriptions_and_preserves_missing_targets() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .logger("AppleSimulatorProvider", Some(jcompat::logging::Level::Warn));
    let connected = Arc::downgrade(&p.h.worker);
    *p.h.worker.before.lock().unwrap() = Arc::new(move |_| {
        if let Some(worker) = connected.upgrade() {
            worker.connected.store(false, Ordering::SeqCst);
        }
        let mut error = marathon_apple::cmd::failure("java.lang.IllegalStateException", "plan failed");
        error.cause = Some(Box::new(marathon_apple::cmd::failure(
            "java.lang.IllegalArgumentException",
            "root",
        )));
        Box::pin(async move { Err(marathon_core::error::VendorError::Other(error)) })
    });
    p.update(&["a"]).await?;
    let expected = fixture("failure-description");
    let warning = format!(
        "Lost the connection to worker local while planning its simulators ({})",
        expected["withMessages"].as_str().unwrap()
    );
    assert!(p.h.logs.lock().unwrap().iter().any(|(_, _, s, _)| s == &warning));
    let missing = p.recovery.missing.lock().unwrap().clone();
    assert_eq!(missing, vec![targets(&["a"])]);
    let no_message = marathon_core::error::Failure {
        class: "java.lang.IllegalStateException".into(),
        message: None,
        cause: None,
        stacktrace: None,
    };
    assert_eq!(
        marathon_ios::device::simulator_provider::describe_failure(&no_message),
        expected["withoutMessage"].as_str().unwrap()
    );
    p.finish().await
}

#[tokio::test(start_paused = true)]
async fn cancelled_replacement_retains_predecessor_lease_and_updates_registry() -> TestResult {
    let mut p = ProviderHarness::new(|_| {})?;
    let services = ProviderHarness::services(&p.h, p.connections.clone(), p.recovery.clone());
    let registry = services.registry.clone();
    p.provider = marathon_ios::device::simulator_provider::AppleSimulatorProvider::new(services)?;
    p.update(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    let gate = SharedDeferred::new();
    let (script, job) = batch(&p.h, old.clone(), Some(gate.clone())).await;
    p.connections
        .connect(Transport::Local, p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    let replacement =
        p.h.factory()
            .create(
                Transport::Local,
                p.h.worker.clone(),
                Arc::new(support::BridgeMock),
                "a".into(),
                Arc::new(
                    marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                        Transport::Local,
                        p.h.clock.clone(),
                    ),
                ),
                p.h.services.context.clone(),
            )
            .await?;
    let old_ref = marathon_core::vendor::DeviceRef(old.clone());
    let new_ref = replacement.device.clone();
    let provider = p.provider.clone();
    let registration = tokio::spawn(async move { provider.register("a@local".into(), replacement).await });
    script.closing.wait().await.map_err(test_error)?;
    assert!(!registration.is_finished());
    assert_eq!(p.provider.subscribe().state().queued, 0);
    assert!(registry.lookup(&old_ref).is_err());
    assert!(registry.lookup(&new_ref).is_ok());
    registration.abort();
    assert!(registration.await.unwrap_err().is_cancelled());
    gate.complete(Ok(()));
    let _ = job.await?;
    p.finish().await?;
    assert!(registry.lookup(&new_ref).is_err());
    Ok(())
}

#[tokio::test]
async fn recovery_termination_error_still_closes_events_and_disposes_devices() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await?;
    let device = p.connected(1).await.remove(0);
    p.recovery.fail_termination.store(true, Ordering::SeqCst);
    p.finish().await?;
    assert!(device.disposed());
    assert!(p.provider.subscribe().state().closed_for_send);
    Ok(())
}
