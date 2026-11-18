//! D92 through the real runner, provider and scheduler with a fake adb peer.

use super::*;
use marathon_adb::client::{AndroidDebugBridgeClient, SocketFactory, shell::v1::ShellCommandRequest};
use marathon_android::tracker::DeviceStateTracker;
use marathon_core::{
    executor::{Signal, bridge::call_vendor, shared::failure},
    listener::{LineListener, LogProducer},
};
use std::sync::atomic::AtomicUsize;

const REMOTE: &str = "/sdcard/artifact";
const SERIAL: &str = "emulator-5554";
const SIBLING: &str = "emulator-5556";

async fn until(mut check: impl FnMut() -> bool) {
    checked(
        tokio::time::timeout(Duration::from_secs(60), async {
            while !check() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await,
    );
}

fn trickled(server: &FakeAdbServer) -> bool {
    server
        .transcript()
        .iter()
        .filter(|r| r["op"] == "trickle" && r["path"] == REMOTE)
        .count()
        >= 2
}

fn peer_closed(server: &FakeAdbServer) -> bool {
    server
        .transcript()
        .iter()
        .any(|r| r["op"] == "peer-eof" && r["path"] == REMOTE)
}

fn trickling_file() -> Value {
    json!([{"path": REMOTE, "content": "interrupted artifact", "recv_cut": "trickle"}])
}

struct LineProbe;
impl LineListener for LineProbe {
    fn on_line<'a>(&'a self, _: &'a str) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Default)]
struct Probe {
    channel: OnceLock<Arc<dyn EventChannel<DeviceEvent>>>,
    device: Mutex<Option<AndroidDevice>>,
    line: OnceLock<Weak<dyn LineListener>>,
    committed_class: OnceLock<String>,
    batches: AtomicUsize,
    pull_ended: AtomicBool,
}

struct PullOnSecondBatch {
    probe: Arc<Probe>,
    path: PathBuf,
}

/// Let setup deliver its device before remote discovery borrows it, with the clock frozen.
struct ReadyProvider {
    inner: Arc<dyn DeviceProvider>,
    initial: OnceLock<DeviceRef>,
}

impl DeviceProvider for ReadyProvider {
    fn initialize(&self, context: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.inner.initialize(context).await?;
            let channel = self.inner.subscribe();
            let Some(DeviceEvent::Connected(device)) = channel.receive().await? else {
                panic!("expected initial device");
            };
            let _ = self.initial.set(device.clone());
            assert!(channel.try_send(DeviceEvent::Connected(device)).is_ok());
            Ok(())
        })
    }

    fn borrow(&self, _: VendorContext) -> VendorFuture<'_, DeviceRef> {
        Box::pin(async move { Ok(self.initial.get().unwrap().clone()) })
    }

    fn terminate(&self) -> VendorFuture<'_, ()> {
        self.inner.terminate()
    }

    fn published_failure(&self) -> Option<Failure> {
        self.inner.published_failure()
    }

    fn subscribe(&self) -> Arc<dyn EventChannel<DeviceEvent>> {
        self.inner.subscribe()
    }
}

impl AndroidBatchExecutor for PullOnSecondBatch {
    fn execute<'a>(
        &'a self,
        device: &'a AndroidDevice,
        configuration: Arc<marathon_config::Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let slots = match self.probe.batches.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    let _ = self.probe.committed_class.set(batch.tests[0].clazz.to_string());
                    ListenerSlots::default()
                }
                _ => ListenerSlots {
                    recorder: Some(Arc::new(HeldPullFactory(self.probe.clone(), self.path.clone()))),
                    ..ListenerSlots::default()
                },
            };
            AndroidRunnerExecutor { slots }
                .execute(device, configuration, pool, batch, result, context)
                .await
        })
    }
}

struct HeldPullFactory(Arc<Probe>, PathBuf);
struct HeldPull(Arc<Probe>, AndroidDevice, PathBuf);
impl AndroidListenerFactory for HeldPullFactory {
    fn create(
        &self,
        device: &AndroidDevice,
        _: &marathon_config::Configuration,
        _: &DevicePoolId,
        _: &BatchRef,
    ) -> VendorResult<InstalledListener> {
        Ok(InstalledListener {
            listener: Arc::new(HeldPull(self.0.clone(), device.clone(), self.1.clone())),
            attachments: None,
        })
    }
}
impl AndroidTestRunListener for HeldPull {
    fn on_event_in<'a>(&'a self, event: &'a TestEvent, mut context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if matches!(event, TestEvent::TestStarted { .. }) {
                let line: Arc<dyn LineListener> = Arc::new(LineProbe);
                let _ = self.0.line.set(Arc::downgrade(&line));
                self.1.add_line_listener(line)?;
                *checked_lock(&self.0.device) = Some(self.1.clone());
                // Model a protected listener that has already admitted an artifact read.
                context.cancellation = Signal::new();
                let outcome = self.1.pull_file(REMOTE, &self.2.to_string_lossy(), context).await;
                self.0.pull_ended.store(true, Ordering::Release);
                outcome?;
            }
            Ok(())
        })
    }
}

fn channel_failure_with_trickling_pull(socket_idle_disabled: bool) {
    let (mut script, _, _) = load("same-server");
    script["devices"][0]["files"] = trickling_file();
    // Put a second stuck request in the forced portion. It gets the remainder, even though the normal shell limit is
    // 20 seconds. The normal marker write still uses the fake's built-in handler.
    script["shell"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"regex": "token=.*; marker=.*; rm -f ", "hang": true}));
    let script = checked(checked(Script::from_yaml(&script.to_string())).compile(Path::new(".")));
    let adb = checked(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build(),
    );
    let mut server = checked(adb.block_on(FakeAdbServer::start(script, ServerOptions::default())));
    let output = checked(tempfile::tempdir());
    let root = output.path().to_owned();
    let path = root.join("abandoned.log");
    let options = Options {
        batch_size: Some(1),
        timeouts: socket_idle_disabled.then(|| (json!({"socketIdleTimeout": "PT0S"}), 180_000)),
        ..Options::default()
    };
    checked(CoreExecutor::run_virtual(move |core, replay| {
        Box::pin(async move {
            let binding = BindingContext {
                profile: Profile::Marathonfile,
                marathonfile: PathBuf::from("/Marathonfile"),
                marathonfile_dir: PathBuf::from("/"),
                process_dir: PathBuf::from("/"),
                environment: vec![],
                now: jcompat::jtime::instant_of(0, 0).unwrap(),
                analytics_tracking: false,
                bugsnag_reporting: false,
            };
            let configuration = Arc::new(checked(
                MarathonConfigurationLoader.parse_sync(&marathonfile(&[server.port()], &root, &options), &binding),
            ));
            let probe = Arc::new(Probe::default());
            let factory_probe = probe.clone();
            let hooks = RuntimeHooks::live();
            let logging = Arc::new(Mutex::new(LogSettings::default()));
            let console = RecordingConsole::new(logging.clone());
            let inputs = RunInputs {
                configuration,
                console: console.clone(),
                logging,
                stderr: Arc::new(|_| {}),
                cwd: PathBuf::from("/"),
                environment: vec![],
                system_information: vec![],
                vendor: Box::new(move |dependencies| {
                    Box::pin(async move {
                        let executor = Arc::new(PullOnSecondBatch {
                            probe: factory_probe.clone(),
                            path,
                        });
                        let mut services = create_services_with_executor(dependencies, executor).await?;
                        let _ = factory_probe.channel.set(services.provider.subscribe());
                        services.provider = Arc::new(ReadyProvider {
                            inner: services.provider,
                            initial: OnceLock::new(),
                        });
                        Ok(services)
                    })
                }),
                metrics: Box::new(|_, _| MetricsBackends {
                    setup: Arc::new(NoMetricsSetup),
                    backend: None,
                }),
                analytics: None,
                extra_reporters: Box::new(marathon_core::construction::standard_reporters),
                barriers: hooks.barriers(),
                hooks,
                timer: None,
                wall: Arc::new(host_millis),
                exit: None,
                signal_trigger: None,
                signal_hooks: Default::default(),
                live_output: false,
            };
            let marathon = construct(core.clone(), inputs).await.map_err(OperationError::Failed)?;
            let run = core.job(JobKind::Ordinary, None);
            let result = Rc::new(RefCell::new(None));
            let published = result.clone();
            core.spawn(
                run.clone(),
                Box::new(move |_| {
                    Box::pin(async move {
                        *published.borrow_mut() = Some(marathon.run(ExecutionCommand::Run).await);
                        Ok(())
                    })
                }),
            );
            until(|| trickled(&server)).await;
            assert_eq!(probe.batches.load(Ordering::Acquire), 2);
            let device = checked_lock(&probe.device).take().unwrap();
            let start = core.vendor_clock().now_millis();
            let cause = failure(FailureKind::IllegalState, "primary device channel failure");
            let before = server.transcript().len();
            probe.channel.get().unwrap().close(Some(cause.clone()));
            checked(replay.drain_current().await);
            let denied = device.add_line_listener(Arc::new(LineProbe));
            let client = device.client().clone();
            let refused = call_vendor(core.vendor().as_ref(), Signal::new(), move |_| {
                Box::pin(async move {
                    client
                        .execute(ShellCommandRequest::new("echo should-not-be-sent"), Some(SERIAL))
                        .await
                        .map(|_| ())
                        .map_err(marathon_android::adb_error)
                })
            })
            .await;
            checked(replay.advance_to(start + 54_999).await);
            assert!(!peer_closed(&server), "grace ended before the reserve");
            checked(replay.advance_to(start + 55_000).await);
            if device.client().socket_factory().is_closed() {
                until(|| peer_closed(&server) && requests(&server, SERIAL, "rm -f \"$marker\"") == 1).await;
                checked(replay.advance_to(start + 59_999).await);
                assert!(
                    result.borrow().is_none(),
                    "marker was still outstanding before the deadline"
                );
            }
            checked(replay.advance_to(start + 65_000).await);
            // Assert the outcome before waiting for TCP EOF, so the unfixed run fails by virtual time.
            let outcome = result.borrow_mut().take().expect("run still waiting after 65 seconds");
            assert!(!checked(outcome));
            assert!(console.lines().iter().any(|line| matches!(
                line, Line::Log { message, cause: Some(actual), .. }
                    if message == "Marathon run failed" && *actual == cause
            )));
            assert_eq!(device.cancellation_cleanup_millis(), Some(60_000));
            assert!(matches!(denied, Err(VendorError::Cancelled(_))));
            assert!(refused.is_err());
            let later: Vec<_> = server.transcript()[before..]
                .iter()
                .filter(|r| r["type"] == "request" && r["serial"] == SERIAL)
                .cloned()
                .collect();
            assert_eq!(later.len(), 1, "{later:?}");
            assert!(later[0]["service"].as_str().unwrap().contains("rm -f \"$marker\""));
            until(|| peer_closed(&server)).await;
            assert!(probe.pull_ended.load(Ordering::Acquire));
            assert!(probe.line.get().unwrap().upgrade().is_none());
            assert!(device.client().socket_factory().is_closed());
            let files = files_under(&root);
            assert!(
                files
                    .keys()
                    .all(|name| !name.contains(".marathon-pull-") && name != "abandoned.log")
            );
            let raw: Value = checked(serde_json::from_slice(&files["test_result/raw.json"]));
            let class = probe.committed_class.get().unwrap();
            assert!(
                raw.as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["class"] == *class && r["status"] == "PASSED"),
                "{raw}"
            );
            checked(server.shutdown().await);
            Ok(())
        })
    }));
}

#[test]
fn a_fatal_channel_cause_outlives_a_trickling_pull_d92() {
    channel_failure_with_trickling_pull(false);
}

#[test]
fn a_fatal_channel_cause_outlives_a_trickling_pull_with_idle_disabled_d92() {
    channel_failure_with_trickling_pull(true);
}

#[test]
fn forcing_one_devices_pull_keeps_its_adb_sibling_working_d92() {
    support::run_with_driven_clock(support::configuration(), |runtime, context, clock| {
        Box::pin(async move {
            let mut spec = support::base_script(33, vec![]);
            let mut sibling = spec["devices"][0].clone();
            sibling["serial"] = json!(SIBLING);
            spec["devices"][0]["files"] = trickling_file();
            spec["devices"].as_array_mut().unwrap().push(sibling);
            spec["shell"]
                .as_array_mut()
                .unwrap()
                .push(json!({"exact": "echo alive", "stdout": "alive"}));
            let mut server = support::server(spec).await;
            let root = Arc::new(SocketFactory::new(10_000, 30_000));
            let client = AndroidDebugBridgeClient::new(server.addr().ip(), server.port(), root.clone(), None);
            let device = |serial: &str| {
                AndroidDevice::new(
                    client.clone(),
                    serial.to_owned(),
                    Arc::new(DeviceStateTracker::default()),
                    runtime.clone(),
                )
            };
            let first = device(SERIAL);
            let sibling = device(SIBLING);
            first.setup(context.clone()).await?;
            sibling.setup(context.clone()).await?;
            support::logcat_streams(&[(&server, 2)]).await;
            let first_line: Arc<dyn LineListener> = Arc::new(LineProbe);
            let removed = Arc::downgrade(&first_line);
            first.add_line_listener(first_line)?;
            let check_sibling = async || {
                assert_eq!(
                    checked(
                        sibling
                            .client()
                            .execute(ShellCommandRequest::new("echo alive"), Some(SIBLING))
                            .await
                    )
                    .output(),
                    "alive"
                );
            };
            check_sibling().await;
            let output = checked(tempfile::tempdir());
            let path = output.path().join("result");
            let receiving = first.clone();
            let pull = runtime.dependencies.io.spawn(Box::pin(async move {
                receiving.pull_file(REMOTE, &path.to_string_lossy(), context).await
            }));
            until(|| trickled(&server)).await;
            let disposing = first.clone();
            let dispose = runtime
                .dependencies
                .io
                .spawn(Box::pin(async move { disposing.dispose().await }));
            clock.advance_to(55_000).await;
            assert!(first.client().socket_factory().is_closed());
            assert!(!root.is_closed());
            until(|| peer_closed(&server)).await;
            assert!(matches!(pull.join().await, Err(VendorError::Cancelled(_))));
            dispose.join().await?;
            assert!(removed.upgrade().is_none());
            check_sibling().await;
            assert!(files_under(output.path()).is_empty());
            sibling.dispose().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
