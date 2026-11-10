//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt
//! and DeviceStateTracker.kt: where Kotlin disconnects a device at its first `offline`, D41 gives it a grace period.
//!
//! The vendor clock does not move in most of these, so a grace period stays open for as long as a test needs and can
//! only end by the device's own state. Whole runs, with grace periods that run out, are in `reconnect_test.rs`.
use marathon_error::FailureKind;
mod support;
use marathon_android::{
    device::{AndroidBatchExecutor, AndroidDevice},
    provider::AdamDeviceProvider,
    services::AndroidRuntime,
};
use marathon_core::{
    error::*,
    executor::shared::SharedDeferred,
    model::{BatchRef, DevicePoolId, NetworkState, TestBatch, TestBatchResults},
    runtime::{Deferred, VendorFuture},
    vendor::{DeviceEvent, DeviceProvider, DeviceRef, VendorContext},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::*;

const SERIAL: &str = "emulator-5554";

async fn until(mut condition: impl FnMut() -> bool) {
    checked(
        tokio::time::timeout(Duration::from_secs(60), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await,
    );
}
async fn event(provider: &AdamDeviceProvider) -> DeviceEvent {
    let channel = provider.subscribe();
    checked(checked(
        tokio::time::timeout(Duration::from_secs(60), channel.receive()).await,
    ))
    .unwrap()
}
/// An event already waiting, without waiting for one.
async fn pending_event(provider: &AdamDeviceProvider) -> Option<DeviceEvent> {
    let channel = provider.subscribe();
    tokio::time::timeout(Duration::ZERO, channel.receive())
        .await
        .ok()
        .and_then(checked)
}
fn getprops(server: &marathon_adb::server::FakeAdbServer) -> usize {
    commands(server).iter().filter(|c| *c == "getprop").count()
}
fn in_grace(device: &DeviceRef) -> bool {
    !device.0.info().healthy
}
async fn connected(provider: &AdamDeviceProvider) -> DeviceRef {
    let DeviceEvent::Connected(device) = event(provider).await else {
        panic!("connection")
    };
    device
}

/// The state a real emulator reports when adbd restarts, here in its full form: the empty list in the middle is why a
/// device missing from the list during its grace is not simply gone. No event at all, the same device object, the same
/// actor; only what adb closed with the transport is set up again, after one `getprop` to see that it did not restart.
#[test]
fn a_brief_offline_keeps_the_device_d41() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            let props = getprops(&server);
            let android = provider.android_device(&device).unwrap();
            assert_eq!(android.grace_periods(), 0);

            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&device)).await;
            // D24: adb says it is not connected, and an attempt ending now says the same.
            assert_eq!(device.0.info().network_state, NetworkState::Disconnected);
            checked(server.disconnect_device(SERIAL));
            checked(server.connect_device(SERIAL));
            assert!(pending_event(&provider).await.is_none());
            checked(server.set_device_state(SERIAL, "device"));

            logcat_streams(&[(&server, 2)]).await;
            until(|| !in_grace(&device)).await;
            assert_eq!(device.0.info().network_state, NetworkState::Connected);
            assert!(pending_event(&provider).await.is_none());
            assert_eq!(android.grace_periods(), 1);
            assert_eq!(getprops(&server), props + 1);
            assert_eq!(provider.android_device(&device).unwrap().adb_serial(), SERIAL);

            // The same device is still subject to the normal route.
            checked(server.disconnect_device(SERIAL));
            let DeviceEvent::Disconnected(gone) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(gone, device);
            assert_eq!(server.warnings(), 0, "{:?}", server.transcript());
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// A healthy device's list passes untouched, and termination with a grace period open needs nothing from its timer,
/// which here never fires.
#[test]
fn termination_cancels_an_open_grace_d41() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&device)).await;
            checked(tokio::time::timeout(Duration::from_secs(60), provider.terminate()).await)?;
            assert!(pending_event(&provider).await.is_none());
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// A device that has not finished setup has no actor to keep, and its setup has failed by now: it goes through disconnect
/// and connect, as it always did, and is set up.
#[test]
fn a_device_in_setup_has_no_grace_d41() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            let rules = script["shell"].as_array_mut().unwrap();
            rules.insert(0, serde_json::json!({"exact":"dumpsys input","hang":true,"times":1}));
            let mut server = server(script).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            until(|| commands(&server).contains(&"dumpsys input".to_owned())).await;
            checked(server.set_device_state(SERIAL, "offline"));
            assert!(matches!(event(&provider).await, DeviceEvent::Disconnected(_)));
            checked(server.set_device_state(SERIAL, "device"));
            let device = connected(&provider).await;
            assert_eq!(provider.android_device(&device).unwrap().grace_periods(), 0);
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// A device that restarted reports `device` again before it has booted (13 s on a real emulator, 15 s before boot
/// completed). It needs setup, as after any reboot, so it takes the disconnect and connect that the first `offline`
/// used to give it.
#[test]
fn a_device_that_returns_restarted_is_set_up_again_d41() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            let props = script["devices"][0]["getprop"].as_object().unwrap().clone();
            let all: String = props
                .iter()
                .map(|(k, v)| format!("[{k}]: [{}]\n", v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
                .collect();
            let rules = script["shell"].as_array_mut().unwrap();
            rules.insert(0, serde_json::json!({"exact":"getprop","times":2,"stdout":all}));
            rules.insert(
                1,
                serde_json::json!({"exact":"getprop","times":1,"stdout":"[ro.build.version.sdk]: [27]\n"}),
            );
            let mut server = server(script).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let first = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&first)).await;
            checked(server.set_device_state(SERIAL, "device"));
            let DeviceEvent::Disconnected(gone) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(gone, first);
            let second = connected(&provider).await;
            assert_ne!(second, first);
            assert!(second.0.info().healthy);
            assert!(!first.0.info().healthy);
            logcat_streams(&[(&server, 2)]).await;
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// Five grace periods at most: the sixth `offline` disconnects the device, with no timer needed.
#[test]
fn grace_periods_are_bounded_per_device_d41() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            let android = provider.android_device(&device).unwrap();
            for round in 1..=5 {
                checked(server.set_device_state(SERIAL, "offline"));
                until(|| in_grace(&device)).await;
                assert_eq!(android.grace_periods(), round);
                checked(server.set_device_state(SERIAL, "device"));
                logcat_streams(&[(&server, 1 + round as usize)]).await;
                until(|| !in_grace(&device)).await;
            }
            assert!(pending_event(&provider).await.is_none());
            checked(server.set_device_state(SERIAL, "offline"));
            let DeviceEvent::Disconnected(gone) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(gone, device);
            assert_eq!(android.grace_periods(), 5);
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// Another state that is not passing ends the grace at once. `unauthorized` waits for a person, and is no blip.
#[test]
fn a_state_that_is_not_passing_ends_the_grace_d41() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&device)).await;
            checked(server.set_device_state(SERIAL, "authorizing"));
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(pending_event(&provider).await.is_none());
            checked(server.set_device_state(SERIAL, "unauthorized"));
            let DeviceEvent::Disconnected(gone) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(gone, device);
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// D18: the same serial on two servers. A grace period on one reaches neither the other's device nor its gate, and its
/// count is its own.
#[test]
fn a_grace_on_one_server_leaves_the_other_alone_d41() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut one = server(base_script(27, vec![])).await;
            let mut other = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[one.port(), other.port()]))?;
            provider.initialize(context).await?;
            let first = connected(&provider).await;
            let second = connected(&provider).await;
            logcat_streams(&[(&one, 1), (&other, 1)]).await;
            let port = |device: &DeviceRef| provider.android_device(device).unwrap().client().port();
            let (on_one, on_other) = if port(&first) == one.port() {
                (first, second)
            } else {
                (second, first)
            };
            checked(one.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&on_one)).await;
            assert!(on_other.0.info().healthy);
            assert_eq!(provider.android_device(&on_other).unwrap().grace_periods(), 0);
            // Only the other server's device can be borrowed now.
            let borrowed = provider
                .borrow(marathon_core::vendor::VendorContext {
                    cancellation: marathon_core::executor::Signal::new(),
                    clock: runtime.dependencies.clock.clone(),
                    hooks: runtime.dependencies.hooks.clone(),
                })
                .await?;
            assert_eq!(borrowed, on_other);
            // The other server's device goes the normal way while this one is held.
            checked(other.disconnect_device(SERIAL));
            let DeviceEvent::Disconnected(gone) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(gone, on_other);
            assert!(in_grace(&on_one));
            checked(one.set_device_state(SERIAL, "device"));
            until(|| !in_grace(&on_one)).await;
            assert!(pending_event(&provider).await.is_none());
            provider.terminate().await?;
            checked(one.shutdown().await);
            checked(other.shutdown().await);
            Ok(())
        })
    });
}

/// A batch whose executor fails with a device loss when told to, like a request caught by the blip.
#[derive(Default)]
struct Lost(tokio::sync::Notify, AtomicUsize);
impl AndroidBatchExecutor for Lost {
    fn execute<'a>(
        &'a self,
        _: &'a AndroidDevice,
        _: Arc<marathon_config::Configuration>,
        _: DevicePoolId,
        _: BatchRef,
        _: Arc<Deferred<TestBatchResults>>,
        _: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.notified().await;
            Err(VendorError::DeviceLost(Failure {
                kind: FailureKind::DeviceLost,
                message: Some(js("lost")),
                cause: None,
                stacktrace: None,
            }))
        })
    }
}
fn with_executor(runtime: &Arc<AndroidRuntime>, ports: &[u16], executor: Arc<Lost>) -> Arc<AndroidRuntime> {
    let changed = change_runtime(runtime, ports);
    Arc::new(AndroidRuntime {
        dependencies: changed.dependencies.clone(),
        configuration: changed.configuration.clone(),
        file_sync: changed.file_sync.clone(),
        bundles: changed.bundles.clone(),
        identifier: changed.identifier.clone(),
        executor,
        log: changed.log.clone(),
    })
}

/// The trap D22's review found in `execute`, here for a blip. A device loss during a grace period, or from one that has
/// just ended, or just before the tracker hears of one, costs only the batch. New batches wait. A device loss with adb
/// saying `device` throughout, on a device that does not answer (D49), is still one.
#[test]
fn a_blip_fails_only_the_batch_and_later_batches_wait_d41() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(stops_answering_after_setup(base_script(27, vec![]))).await;
            let lost = Arc::new(Lost::default());
            let provider = AdamDeviceProvider::new(with_executor(&runtime, &[server.port()], lost.clone()))?;
            provider.initialize(context.clone()).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            let batch = || {
                let device = device.clone();
                let configuration = runtime.dependencies.configuration.clone();
                let context = context.clone();
                tokio::spawn(async move {
                    device
                        .0
                        .execute(
                            configuration,
                            DevicePoolId { name: "pool".into() },
                            Arc::new(TestBatch {
                                tests: vec![],
                                id: "batch".into(),
                            }),
                            Arc::new(SharedDeferred::default()),
                            context,
                        )
                        .await
                })
            };
            let outcome = |running: tokio::task::JoinHandle<VendorResult<()>>| async move {
                checked(checked(tokio::time::timeout(Duration::from_secs(60), running).await))
            };
            let entered = |count: usize| {
                let lost = lost.clone();
                async move { until(|| lost.1.load(Ordering::SeqCst) >= count).await }
            };

            // No blip: a device loss.
            let running = batch();
            entered(1).await;
            lost.0.notify_one();
            assert!(matches!(outcome(running).await, Err(VendorError::DeviceLost(_))));

            // During a grace period. The batch does not return until that is over, so that the actor asks for no next
            // batch, which would be charged to its tests if the device never came back.
            let running = batch();
            entered(2).await;
            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&device)).await;
            lost.0.notify_one();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(!running.is_finished());
            checked(server.set_device_state(SERIAL, "device"));
            assert!(matches!(outcome(running).await, Err(VendorError::BatchExecution(_))));
            logcat_streams(&[(&server, 2)]).await;
            until(|| !in_grace(&device)).await;

            // A batch given to the device just before a blip waits at the gate, which uses up nothing, and runs after it.
            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&device)).await;
            let waiting = batch();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(!waiting.is_finished());
            assert_eq!(lost.1.load(Ordering::SeqCst), 2);
            checked(server.set_device_state(SERIAL, "device"));
            entered(3).await;
            logcat_streams(&[(&server, 3)]).await;
            until(|| !in_grace(&device)).await;

            // A grace period that began and ended during this batch.
            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&device)).await;
            checked(server.set_device_state(SERIAL, "device"));
            logcat_streams(&[(&server, 4)]).await;
            until(|| !in_grace(&device)).await;
            lost.0.notify_one();
            assert!(matches!(outcome(waiting).await, Err(VendorError::BatchExecution(_))));

            // A device loss reported while adb says `offline`, whether or not the tracker has heard yet.
            let running = batch();
            entered(4).await;
            checked(server.set_device_state(SERIAL, "offline"));
            lost.0.notify_one();
            until(|| in_grace(&device)).await;
            checked(server.set_device_state(SERIAL, "device"));
            assert!(matches!(outcome(running).await, Err(VendorError::BatchExecution(_))));
            logcat_streams(&[(&server, 5)]).await;
            until(|| !in_grace(&device)).await;

            // And cancelling a batch at the gate ends it.
            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&device)).await;
            let signal = marathon_core::executor::Signal::new();
            let gated = {
                let device = device.clone();
                let configuration = runtime.dependencies.configuration.clone();
                let context = VendorContext {
                    cancellation: signal.clone(),
                    ..context.clone()
                };
                tokio::spawn(async move {
                    device
                        .0
                        .execute(
                            configuration,
                            DevicePoolId { name: "pool".into() },
                            Arc::new(TestBatch {
                                tests: vec![],
                                id: "batch".into(),
                            }),
                            Arc::new(SharedDeferred::default()),
                            context,
                        )
                        .await
                })
            };
            tokio::time::sleep(Duration::from_millis(20)).await;
            signal.cancel(marathon_core::executor::shared::requested("test"));
            assert!(matches!(outcome(gated).await, Err(VendorError::Cancelled(_))));
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// D22 with D41: a server outage during a grace period closes that grace, and its window decides. A device still offline
/// when the window ends is disconnected there and then, with no second grace on top of the outage's bound, and only once.
#[test]
fn an_outage_does_not_add_a_grace_to_its_bound_d41() {
    let _ports = support_ports();
    let mut c = configuration();
    android(&mut c).wait_for_devices_timeout_millis = 1500;
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            let android = provider.android_device(&device).unwrap();
            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&device)).await;
            server.kill(Some(Duration::from_millis(100)));
            let DeviceEvent::Disconnected(gone) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(gone, device);
            assert_eq!(android.grace_periods(), 1);
            until(|| android.adb_server_available()).await;
            assert!(pending_event(&provider).await.is_none());
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
/// A killed fake server listens on its port again, so nothing else may bind it meanwhile (`docs/testing.md`, rule 3).
static PORTS: std::sync::RwLock<()> = std::sync::RwLock::new(());
fn shared_ports() -> std::sync::RwLockReadGuard<'static, ()> {
    PORTS.read().unwrap_or_else(|e| e.into_inner())
}
fn support_ports() -> std::sync::RwLockWriteGuard<'static, ()> {
    PORTS.write().unwrap_or_else(|e| e.into_inner())
}

/// Remote parsing has three attempts with no delay, which a brief `offline` would use up in its first moments. Here each
/// of three blips cuts an attempt off, and the fourth attempt finishes the list.
#[test]
fn remote_parsing_waits_out_a_brief_offline_d41() {
    let _ports = shared_ports();
    let test = |code| {
        format!(
            "INSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test=one\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: {code}\n"
        )
    };
    let mut c = configuration();
    let a = android(&mut c);
    a.test_application_output = Some(std::path::PathBuf::from(
        fixture("vendor-android/apk/app-debug-androidTest.apk")
            .to_string_lossy()
            .into_owned(),
    ));
    a.test_parser_configuration = marathon_config::vendor::AndroidTestParserConfiguration::Remote {
        instrumentation_args: vec![],
    };
    c.test_output_timeout_millis = 60_000;
    run_with_stopped_clock(c, move |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(30, vec!["shell_v2", "cmd"]);
            let rules = script["shell"].as_array_mut().unwrap();
            rules.push(serde_json::json!({"exact":"pm list packages","stdout":""}));
            rules.push(serde_json::json!({"prefix":"cmd package install","service":"exec","stdout":"Success\n"}));
            rules.push(serde_json::json!({"prefix":"am instrument","stdout":test(1),"hang":true,"times":3}));
            rules.push(serde_json::json!({"prefix":"am instrument","stdout":format!("{}{}INSTRUMENTATION_CODE: -1\n", test(1), test(0))}));
            let mut server = server(script).await;
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
            provider.initialize(context.clone()).await?;
            let device = connected(&provider).await;
            let android = provider.android_device(&device).unwrap();
            logcat_streams(&[(&server, 1)]).await;
            let parser = marathon_android::remote_parser::AmInstrumentTestParser {
                runtime,
                provider: provider.clone(),
            };
            let parse = tokio::spawn({
                let (device, context) = (device.clone(), context.clone());
                async move {
                    use marathon_core::vendor::RemoteTestParser;
                    parser.extract(device, context).await
                }
            });
            let instruments = || {
                commands(&server)
                    .iter()
                    .filter(|c| c.starts_with("am instrument"))
                    .count()
            };
            for attempt in 1..=3 {
                until(|| instruments() >= attempt).await;
                tokio::time::sleep(Duration::from_millis(50)).await;
                checked(server.set_device_state(SERIAL, "offline"));
                until(|| android.grace_periods() == attempt as u32).await;
                // No attempt starts while the device is away.
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert_eq!(instruments(), attempt);
                checked(server.set_device_state(SERIAL, "device"));
            }
            let tests = checked(checked(tokio::time::timeout(Duration::from_secs(60), parse).await))?;
            assert_eq!(tests.len(), 1);
            assert_eq!(instruments(), 4);
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// D49 (N15): a request rejected or closed before its reply, on a device the server lists as `device` and which answers,
/// costs the batch and not the device, three times in a run. The tracker sees no change here, so a device retired now
/// would stay out for good. No disconnect or connect is seen, and one `getprop` per loss is all it sends.
#[test]
fn a_lost_request_on_a_listed_device_that_answers_keeps_it_up_to_a_limit_d49() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let lost = Arc::new(Lost::default());
            let provider = AdamDeviceProvider::new(with_executor(&runtime, &[server.port()], lost.clone()))?;
            provider.initialize(context.clone()).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            let getprops = |server: &marathon_adb::server::FakeAdbServer| {
                server
                    .transcript()
                    .iter()
                    .filter(|r| r["type"] == "shell" && r["command"] == "getprop")
                    .count()
            };
            let before = getprops(&server);
            for number in 1..=4 {
                let running = {
                    let device = device.clone();
                    let configuration = runtime.dependencies.configuration.clone();
                    let context = context.clone();
                    tokio::spawn(async move {
                        device
                            .0
                            .execute(
                                configuration,
                                DevicePoolId { name: "pool".into() },
                                Arc::new(TestBatch {
                                    tests: vec![],
                                    id: "batch".into(),
                                }),
                                Arc::new(SharedDeferred::default()),
                                context,
                            )
                            .await
                    })
                };
                until(|| lost.1.load(Ordering::SeqCst) >= number).await;
                lost.0.notify_one();
                let outcome = checked(checked(tokio::time::timeout(Duration::from_secs(60), running).await));
                if number <= 3 {
                    assert!(
                        matches!(outcome, Err(VendorError::BatchExecution(_))),
                        "{number}: {outcome:?}"
                    );
                } else {
                    assert!(
                        matches!(outcome, Err(VendorError::DeviceLost(_))),
                        "{number}: {outcome:?}"
                    );
                }
            }
            // The limit is checked before the question, so the loss that retires it sends none.
            assert_eq!(getprops(&server), before + 3);
            assert!(
                device.0.info().healthy,
                "this device was retired by its actor, not disconnected"
            );
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// D49: a device that stopped answering goes at its first lost request, though adb still lists it.
#[test]
fn a_lost_request_on_a_listed_device_that_does_not_answer_loses_it_d49() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(stops_answering_after_setup(base_script(27, vec![]))).await;
            let lost = Arc::new(Lost::default());
            let provider = AdamDeviceProvider::new(with_executor(&runtime, &[server.port()], lost.clone()))?;
            provider.initialize(context.clone()).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            let running = {
                let device = device.clone();
                let configuration = runtime.dependencies.configuration.clone();
                let context = context.clone();
                tokio::spawn(async move {
                    device
                        .0
                        .execute(
                            configuration,
                            DevicePoolId { name: "pool".into() },
                            Arc::new(TestBatch {
                                tests: vec![],
                                id: "batch".into(),
                            }),
                            Arc::new(SharedDeferred::default()),
                            context,
                        )
                        .await
                })
            };
            until(|| lost.1.load(Ordering::SeqCst) >= 1).await;
            lost.0.notify_one();
            let outcome = checked(checked(tokio::time::timeout(Duration::from_secs(60), running).await));
            assert!(matches!(outcome, Err(VendorError::DeviceLost(_))), "{outcome:?}");
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// D49: one that has left the list goes at once too; the tracker's own disconnect takes it from there.
#[test]
fn a_lost_request_on_a_device_no_longer_listed_loses_it_d49() {
    let _ports = shared_ports();
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let lost = Arc::new(Lost::default());
            let provider = AdamDeviceProvider::new(with_executor(&runtime, &[server.port()], lost.clone()))?;
            provider.initialize(context.clone()).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            let running = {
                let device = device.clone();
                let configuration = runtime.dependencies.configuration.clone();
                let context = context.clone();
                tokio::spawn(async move {
                    device
                        .0
                        .execute(
                            configuration,
                            DevicePoolId { name: "pool".into() },
                            Arc::new(TestBatch {
                                tests: vec![],
                                id: "batch".into(),
                            }),
                            Arc::new(SharedDeferred::default()),
                            context,
                        )
                        .await
                })
            };
            until(|| lost.1.load(Ordering::SeqCst) >= 1).await;
            checked(server.disconnect_device(SERIAL));
            lost.0.notify_one();
            let outcome = checked(checked(tokio::time::timeout(Duration::from_secs(60), running).await));
            assert!(matches!(outcome, Err(VendorError::DeviceLost(_))), "{outcome:?}");
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
