//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//! WP 5.2's in-process 0.4b setup/prepare/batch/dispose witness.
mod simulator_support;
mod support;
use fake_worker::{
    Config, RunningWorker,
    apple::{Scenario, World},
    fs::VirtualFs,
    router::Router,
    script::{Action, Faults, Matcher, Rule, Script},
    server::AuthPolicy,
};
use marathon_apple::{
    cmd::{CommandExecutor, CommandRequest, CommandSession},
    device::connection_factory::{ConnectionError, ConnectionFactory, Executor},
};
use marathon_config::{
    devices::Transport,
    vendor::{LifecycleAction, SshAuthentication},
};
use marathon_core::{
    error::{VendorError, VendorResult},
    runtime::{CancellationSignal, VendorClock, VendorFuture, VendorTimeoutBody},
};
use marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext;
use marathon_logging::Level;
use marathon_test_support::apple::WorldExecutor;
use serde_json::Value;
use simulator_support::*;
use std::{path::Path, sync::Arc, time::Duration};
use support::{Clock, context, duration, watchdog};
use tokio::sync::Semaphore;
fn add_runner_app(h: &mut Harness) -> TestResult {
    let runner = h.dir.path().join("runner.app");
    std::fs::create_dir_all(runner.join("PlugIns"))?;
    std::fs::write(
        runner.join("Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>com.example.Runner</string><key>CFBundleName</key><string>Runner</string><key>CFBundleExecutable</key><string>Runner</string><key>CFBundleSupportedPlatforms</key><array><string>iPhoneSimulator</string></array></dict></plist>",
    )?;
    std::fs::write(runner.join("Runner"), [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0])?;
    std::fs::rename(h.dir.path().join("tests.xctest"), runner.join("PlugIns/tests.xctest"))?;
    let bundle = Arc::make_mut(&mut h.services.vendor).bundle.as_mut().unwrap();
    bundle.test_app = Some(std::path::PathBuf::from(runner.to_str().unwrap()));
    bundle.xctest = Some(std::path::PathBuf::from(
        runner.join("PlugIns/tests.xctest").to_str().unwrap(),
    ));
    Arc::make_mut(&mut h.services.configuration).vendor_configuration =
        marathon_config::VendorConfiguration::Ios((*h.services.vendor).clone());
    Ok(())
}

#[tokio::test]
async fn setup_prepare_batch_and_dispose_share_the_in_process_apple_world() -> TestResult {
    let mut h = Harness::new(|v| {
        v.lifecycle_configuration.on_dispose = vec![LifecycleAction::Shutdown];
        v.xcresult.remote_clean = true;
    })?;
    add_runner_app(&mut h)?;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fake-worker/apple/ios-smoke/script/world.json");
    let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(fixture)?)?;
    scenario.expectations.clear();
    scenario.devices = vec![fake_worker::apple::UDID.into()];
    scenario.target = "Tests".into();
    scenario.tests = vec![
        fake_worker::apple::Test {
            class: "Suite".into(),
            method: "test1".into(),
            status: "passed".into(),
        },
        fake_worker::apple::Test {
            class: "Suite".into(),
            method: "test2".into(),
            status: "failed".into(),
        },
    ];
    let world = World::new(scenario).map_err(|e| e.to_string())?;
    let fs = VirtualFs::create(h.dir.path().join("worker-root"))
        .await
        .map_err(|e| e.to_string())?;
    let router = Router {
        script: Script::default(),
        rsync_remote: "/usr/bin/rsync".into(),
        rsync_binary: "/usr/bin/rsync".into(),
        extensions: vec![Arc::new(world.clone())],
    };
    let worker = Arc::new(WorldExecutor::new(
        h.services.runtime.clone(),
        &mut h.services.logging.registry.lock().unwrap(),
        router,
        fs.clone(),
        h.dir.path().to_owned(),
    ));
    let d = h
        .device_on_bridge(
            worker.clone(),
            fake_worker::apple::UDID,
            Arc::new(WorkerProvisioningContext::new(Transport::Local, h.clock.clone())),
            Arc::new(worker.file_bridge()),
        )
        .await?;
    let prep = h.prepare(&d).await;
    assert!(
        prep.is_ok(),
        "{}; commands={:?}; logs={:?}",
        prep.unwrap_err().failure().to_string().to_owned(),
        worker.commands.lock().unwrap(),
        h.logs.lock().unwrap()
    );
    let (r, results) = support::bounded(20, run(&h, &d)).await;
    assert!(r.is_ok(), "{r:?}");
    let results = results.unwrap();
    assert_eq!(results.finished.len(), 1);
    assert_eq!(results.failed.len(), 1);
    h.finish(&[d]).await;
    worker.close().await.unwrap();
    let state = world.final_state(&fs).await.map_err(|e| e.to_string())?;
    assert_eq!(state["devices"][fake_worker::apple::UDID]["state"], "Shutdown");
    assert_eq!(state["devices"][fake_worker::apple::UDID]["keyboard"], false);
    assert_eq!(state["sessions"][0]["state"], "finished");
    assert!(
        !state["devices"][fake_worker::apple::UDID]["installed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    world.validate().map_err(|e| e.to_string())?;
    Ok(())
}

struct CloseClock {
    clock: Clock,
    term: Semaphore,
    close: Semaphore,
}
impl VendorClock for CloseClock {
    fn now_millis(&self) -> i64 {
        self.clock.now_millis()
    }
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        let gate = match millis {
            20_000 => &self.term,
            10_000 => &self.close,
            _ => return self.clock.delay(millis, cancellation),
        };
        Box::pin(async move {
            tokio::select! {
                reason = cancellation.cancelled() => Err(VendorError::Cancelled(reason)),
                permit = gate.acquire() => {
                    permit.unwrap().forget();
                    Ok(())
                }
            }
        })
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        self.clock.with_timeout(parent, millis, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        self.clock.with_timeout_or_null(parent, millis, body)
    }
}

struct WireWorker {
    worker: RunningWorker,
    transcript: std::path::PathBuf,
}
impl WireWorker {
    async fn events(&self, name: &str) -> TestResult<Vec<Value>> {
        self.worker.transcript.flush().await.map_err(|e| e.to_string())?;
        let transcript = tokio::fs::read_to_string(&self.transcript).await?;
        let mut events = Vec::new();
        // The worker can be appending another row while this test reads the flushed prefix.
        for line in transcript.split_inclusive('\n').filter(|line| line.ends_with('\n')) {
            let event: Value = serde_json::from_str(line)?;
            if event["event"] == name {
                events.push(event);
            }
        }
        Ok(events)
    }
    async fn wait_for(&self, name: &str, predicate: impl Fn(&Value) -> bool) -> TestResult {
        watchdog(async {
            while !self.events(name).await?.iter().any(&predicate) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        })
        .await
    }
    async fn abandon(&self, executor: &Executor, clock: &CloseClock, connection: u64) -> TestResult {
        let session = executor.execute(held("stubborn"), context()).await.map_err(describe)?;
        let command = fake_worker::transcript::hash(b"stubborn");
        let events = self.events("exec").await?;
        let channel = events
            .iter()
            .find(|line| line["connection"] == connection && line["detail"]["sha256"] == command)
            .expect("the worker acknowledged this exec")["channel"]
            .clone();
        let close = session.close();
        let events = async {
            self.wait_for("signal", |line| {
                line["connection"] == connection && line["channel"] == channel && line["detail"]["signal"] == "TERM"
            })
            .await?;
            clock.term.add_permits(1);
            self.wait_for("client_close", |line| {
                line["connection"] == connection
                    && line["channel"] == channel
                    && line["detail"]["reply_withheld"] == true
            })
            .await?;
            clock.close.add_permits(1);
            TestResult::Ok(())
        };
        let (closed, ()) = watchdog(async { tokio::try_join!(async { Ok(close.await) }, events) }).await?;
        let failure = closed.unwrap_err();
        assert!(failure.failure().to_string().contains("Timeout expired"), "{failure:?}");
        Ok(())
    }
}

fn held(command: &str) -> CommandRequest {
    CommandRequest::new([command], duration(600_000))
}

fn describe(error: VendorError) -> String {
    error.failure().to_string().to_owned()
}

async fn refusal(executor: &Executor) -> VendorResult<()> {
    assert_eq!(executor.safe_execute(held("probe"), context()).await?, None);
    assert!(executor.connected());
    Ok(())
}

async fn start_wire_worker(h: &Harness) -> TestResult<(WireWorker, World)> {
    start_wire_worker_with(h, None).await
}
/// `start_wire_worker`, whose first session open of a connection is answered `late_open` late (D93 round 8).
async fn start_wire_worker_with(
    h: &Harness,
    late_open: Option<fake_worker::script::LateOpen>,
) -> TestResult<(WireWorker, World)> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(
        fixtures.join("fake-worker/apple/ios-smoke/script/world.json"),
    )?)?;
    scenario.expectations.clear();
    scenario.devices = vec![fake_worker::apple::UDID.into()];
    let world = World::new(scenario).map_err(|e| e.to_string())?;
    let hang = |stubborn| Action::Output {
        chunks: vec![],
        status: 0,
        signal: None,
        hang: true,
        ignore_signals: if stubborn {
            vec!["TERM".into(), "KILL".into()]
        } else {
            vec![]
        },
        eof_before_exit: false,
        close_before_eof: false,
        withhold_close: stubborn,
    };
    let script = Script {
        rules: [
            ("healthy", hang(false)),
            ("stubborn", hang(true)),
            ("probe", Action::output("ok\n", 0)),
        ]
        .into_iter()
        .map(|(name, action)| Rule {
            name: name.into(),
            matcher: Matcher::Exact(name.into()),
            action,
        })
        .collect(),
        faults: Faults {
            hold_withheld_session_slots: true,
            late_open,
            ..Default::default()
        },
        ..Default::default()
    };
    let transcript = h.dir.path().join("ssh.jsonl");
    let worker = fake_worker::start(
        Config {
            listen: "127.0.0.1:0".parse()?,
            root: h.dir.path().join("ssh-root"),
            transcript: transcript.clone(),
            host_key: fixtures.join("ssh-keys/ssh-key-0.7.0-rc.11/id_ed25519"),
            auth: AuthPolicy {
                user: "fixture".into(),
                password: Some("fake-password".into()),
                authorized_keys: vec![],
                keyboard_interactive: false,
                password_auth: true,
            },
            max_sessions: 2,
            rsync_binary: "/usr/bin/rsync".into(),
            rsync_remote: "/usr/bin/rsync".into(),
            script,
            fixtures,
            run: "d90".into(),
        },
        vec![Arc::new(world.clone())],
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok((WireWorker { worker, transcript }, world))
}

async fn finish_healthy(session: CommandSession) -> VendorResult<()> {
    session.terminate()?;
    assert_eq!(watchdog(session.await_result(&context())).await?.exit_code, Some(143));
    session.close().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_saturated_shared_worker_rotates_and_prepares_a_simulator_without_ending_old_sessions_d90() -> TestResult {
    watchdog(async {
        let mut h = Harness::new(|vendor| {
            vendor.timeout_configuration.boot_wait = duration(60_000);
        })?;
        add_runner_app(&mut h)?;
        let (wire, world) = start_wire_worker(&h).await?;
        let transport = Transport::Ssh {
            addr: "127.0.0.1".into(),
            port: i32::from(wire.worker.ready.endpoint.port()),
            authentication: Some(SshAuthentication::Password {
                username: "fixture".into(),
                password: "fake-password".into(),
            }),
            check_reachability: false,
        };
        let clock = Arc::new(CloseClock {
            clock: Clock::default(),
            term: Semaphore::new(0),
            close: Semaphore::new(0),
        });
        let mut ssh = h.services.vendor.ssh.clone();
        ssh.share_worker_connection = true;
        ssh.keep_alive_interval = duration(0);
        let factory = ConnectionFactory::new(
            false,
            ssh,
            h.services.vendor.rsync.clone(),
            duration(60_000),
            h.services.runtime.clone(),
            clock.clone(),
            h.services.logging.clone(),
        )
        .map_err(|e| describe(e.into_vendor()))?;
        let acquire = || async {
            factory
                .connect_worker(transport.clone(), context())
                .await
                .map_err(ConnectionError::into_vendor)
                .map_err(describe)
        };

        let old = acquire().await?;
        let old_peer = acquire().await?;
        assert!(Arc::ptr_eq(&old, &old_peer));
        let healthy = old_peer.execute(held("healthy"), context()).await.map_err(describe)?;
        let additional = old.execute(held("healthy"), context()).await.map_err(describe)?;
        refusal(&old).await.map_err(describe)?;
        let same = acquire().await?;
        assert!(
            Arc::ptr_eq(&same, &old),
            "healthy MaxSessions exhaustion must not rotate"
        );
        factory.dispose(same).await.map_err(describe)?;
        assert_eq!(wire.events("connection").await?.len(), 1);
        finish_healthy(additional).await.map_err(describe)?;

        wire.abandon(&old, &clock, 1).await?;
        // Abandonment by itself still permits sharing. A later capacity refusal is the trigger.
        let same = acquire().await?;
        assert!(Arc::ptr_eq(&same, &old));
        factory.dispose(same).await.map_err(describe)?;
        refusal(&old).await.map_err(describe)?;
        refusal(&old).await.map_err(describe)?;

        let (first, second, third) = tokio::join!(acquire(), acquire(), acquire());
        let (first, second, third) = (first?, second?, third?);
        assert!(
            !Arc::ptr_eq(&old, &first),
            "a capacity refusal after abandonment must rotate"
        );
        assert!(Arc::ptr_eq(&first, &second) && Arc::ptr_eq(&first, &third));
        assert_eq!(wire.events("connection").await?.len(), 2);
        assert!(wire.events("connection_end").await?.is_empty());

        // The simulator uses the replacement's SSH executor. File transfers are in-process against the same worker FS.
        let bridge = WorldExecutor::new(
            h.services.runtime.clone(),
            &mut h.services.logging.registry.lock().unwrap(),
            Router {
                script: Script::default(),
                rsync_remote: "/usr/bin/rsync".into(),
                rsync_binary: "/usr/bin/rsync".into(),
                extensions: vec![Arc::new(world)],
            },
            wire.worker.fs.clone(),
            h.dir.path().to_owned(),
        );
        let device = h
            .device_on_bridge(
                first.clone(),
                fake_worker::apple::UDID,
                Arc::new(WorkerProvisioningContext::new(transport.clone(), h.clock.clone())),
                Arc::new(bridge.file_bridge()),
            )
            .await?;
        h.prepare(&device).await.map_err(describe)?;
        h.finish(&[device]).await;
        bridge.close().await.map_err(describe)?;

        let new_healthy = first.execute(held("healthy"), context()).await.map_err(describe)?;
        wire.abandon(&second, &clock, 2).await?;
        refusal(&third).await.map_err(describe)?;
        let capped = acquire().await?;
        assert!(Arc::ptr_eq(&capped, &first));
        assert_eq!(wire.events("connection").await?.len(), 2);
        factory.dispose(capped).await.map_err(describe)?;

        factory.dispose(old.clone()).await.map_err(describe)?;
        assert!(old.connected());
        assert!(wire.events("connection_end").await?.is_empty());
        finish_healthy(healthy).await.map_err(describe)?;
        factory.dispose(old_peer).await.map_err(describe)?;
        wire.wait_for("connection_end", |event| event["connection"] == 1)
            .await?;
        // The peer can record its end before local socket teardown releases the generation slot.
        let latest = loop {
            let lease = acquire().await?;
            if !Arc::ptr_eq(&lease, &first) {
                break lease;
            }
            factory.dispose(lease).await.map_err(describe)?;
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let probe = latest
            .critical_execute(held("probe"), context())
            .await
            .map_err(describe)?;
        assert_eq!(probe.stdout, ["ok"]);
        assert_eq!(wire.events("connection").await?.len(), 3);
        assert_eq!(wire.events("connection_end").await?.len(), 1);

        finish_healthy(new_healthy).await.map_err(describe)?;
        factory.dispose(first.clone()).await.map_err(describe)?;
        factory.dispose(second).await.map_err(describe)?;
        assert!(first.connected());
        factory.dispose(third).await.map_err(describe)?;
        factory.dispose(latest).await.map_err(describe)?;
        factory.join_pending().await.map_err(describe)?;
        for connection in [2, 3] {
            wire.wait_for("connection_end", |event| event["connection"] == connection)
                .await?;
        }
        assert_eq!(wire.events("connection_end").await?.len(), 3);
        let warnings = h
            .logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(target, level, ..)| target == "marathon_apple::ssh_session_open" && *level == Level::WARN)
            .count();
        assert_eq!(
            warnings, 2,
            "warn once per generation that refused a session, with debug off"
        );
        wire.worker.shutdown().await.map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

/// D93 (review round 3, finding 1): over the real SSH executor, the worker refuses the xcodebuild session's channel
/// (both of its `MaxSessions` slots are held), before any exec is sent. Nothing can be running, so the session counts as
/// cleanly ended, and a stopping simulator's verdict can be confirmed (D44's recovery then sets it up again as before).
/// Before, any open failure was unconfirmed, and such a simulator was quarantined and retired.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_refused_before_its_exec_is_confirmed_ended_over_ssh_d93() -> TestResult {
    watchdog(async {
        let h = Harness::new(|_| {})?;
        let (wire, _world) = start_wire_worker(&h).await?;
        let transport = Transport::Ssh {
            addr: "127.0.0.1".into(),
            port: i32::from(wire.worker.ready.endpoint.port()),
            authentication: Some(SshAuthentication::Password {
                username: "fixture".into(),
                password: "fake-password".into(),
            }),
            check_reachability: false,
        };
        let mut ssh = h.services.vendor.ssh.clone();
        ssh.keep_alive_interval = duration(0);
        let factory = ConnectionFactory::new(
            false,
            ssh,
            h.services.vendor.rsync.clone(),
            duration(60_000),
            h.services.runtime.clone(),
            Arc::new(Clock::default()),
            h.services.logging.clone(),
        )
        .map_err(|e| describe(e.into_vendor()))?;
        let executor = factory
            .connect_worker(transport.clone(), context())
            .await
            .map_err(ConnectionError::into_vendor)
            .map_err(describe)?;
        let first = executor.execute(held("healthy"), context()).await.map_err(describe)?;
        let second = executor.execute(held("healthy"), context()).await.map_err(describe)?;
        refusal(&executor).await.map_err(describe)?;
        let session = marathon_apple::xcodebuild_session::XcodebuildSession::new(
            h.services.runtime.clone(),
            h.services.logging.clone(),
        );
        let opener = executor.clone();
        let events = session.produce(
            context(),
            move |ctx| async move { opener.execute(held("xcodebuild"), ctx).await },
            |_, _, _| async { Ok(()) },
        );
        // The open's failure reaches the reader: the open was tried, and refused.
        let opened = events.receive(&context()).await;
        assert!(
            matches!(&opened, Err(error) if !matches!(error, VendorError::Cancelled(_))),
            "{opened:?}"
        );
        let _ = session.stop(duration(10_000), &context()).await;
        assert_eq!(
            session.ended(),
            Some(true),
            "a refused channel was not taken as having started nothing"
        );
        finish_healthy(first).await.map_err(describe)?;
        finish_healthy(second).await.map_err(describe)?;
        factory.dispose(executor).await.map_err(describe)?;
        factory.join_pending().await.map_err(describe)?;
        TestResult::Ok(())
    })
    .await
}

/// D93 (round 8, finding 1): a device's command over the real SSH executor whose channel the worker accepts only after
/// 2 s; the run's stop seals the device meanwhile. After the channel is accepted the executor looks at the device's
/// command gate again, closes the channel and sends no exec: the worker never runs the command, and the refusal is the
/// seal's (a cancellation) with nothing started. Before, the gate was looked at only before the channel was asked for,
/// and the worker ran the command after the seal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_whose_channel_is_accepted_after_the_seal_is_not_sent_d93() -> TestResult {
    watchdog(async {
        let h = Harness::new(|_| {})?;
        let (wire, _world) = start_wire_worker_with(
            &h,
            Some(fake_worker::script::LateOpen {
                delay_millis: 2_000,
                reject: false,
            }),
        )
        .await?;
        let transport = Transport::Ssh {
            addr: "127.0.0.1".into(),
            port: i32::from(wire.worker.ready.endpoint.port()),
            authentication: Some(SshAuthentication::Password {
                username: "fixture".into(),
                password: "fake-password".into(),
            }),
            check_reachability: false,
        };
        let mut ssh = h.services.vendor.ssh.clone();
        ssh.keep_alive_interval = duration(0);
        let factory = ConnectionFactory::new(
            false,
            ssh,
            h.services.vendor.rsync.clone(),
            duration(60_000),
            h.services.runtime.clone(),
            Arc::new(Clock::default()),
            h.services.logging.clone(),
        )
        .map_err(|e| describe(e.into_vendor()))?;
        let executor = factory
            .connect_worker(transport.clone(), context())
            .await
            .map_err(ConnectionError::into_vendor)
            .map_err(describe)?;
        let commands = marathon_apple::cmd::DeviceCommands::new(executor.clone());
        let sent = {
            let commands = commands.clone();
            tokio::spawn(async move { commands.execute(held("probe"), context()).await })
        };
        // The channel open is on the wire (the worker logs it as it takes it) and waits for its late reply.
        wire.wait_for("session_open", |_| true).await?;
        assert!(
            wire.events("late_open_reply").await?.is_empty(),
            "the late reply came before the seal"
        );
        commands.seal(
            marathon_apple::cmd::SealCause::RunStop,
            marathon_apple::cmd::failure(marathon_error::FailureKind::IllegalState, "Device x is stopping"),
        );
        let outcome = sent.await?;
        assert!(matches!(outcome, Err(VendorError::Cancelled(_))), "{:?}", outcome.err());
        let probe = fake_worker::transcript::hash(b"probe");
        let execs = wire.events("exec").await?;
        assert!(
            execs.iter().all(|line| line["detail"]["sha256"] != probe),
            "the worker ran the command after the seal: {execs:?}"
        );
        factory.dispose(executor).await.map_err(describe)?;
        factory.join_pending().await.map_err(describe)?;
        TestResult::Ok(())
    })
    .await
}
