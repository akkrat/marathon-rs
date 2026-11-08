//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/listener/ResultBundleRunListenerTest.kt
mod support;
use jcompat::{
    file_manager::FileManager,
    jtime::Duration,
    logging::{Logger, MarathonLogging},
};
use marathon_apple::{
    apple_device::AppleDevice,
    artifact_processor::ArtifactProcessor,
    bin::AppleBinaryEnvironment,
    cmd::{CommandResult, remote::ssh::sshj::config::SshLogging},
    listener::{AppleTestRunListener, ResultBundleRunListener},
    model::Sdk,
    remote_file_manager::RemoteFileManager,
    worker_commands::WorkerCommands,
};
use marathon_config::{Configuration, devices::Transport, vendor::*};
use marathon_core::{
    error::{VendorError, VendorResult},
    executor::{
        CoreExecutor,
        bridge::call_vendor,
        shared::{SharedDeferred, Signal, failure, requested},
    },
    listener::{LineListener, LogProducer},
    model::{BatchRef, DeviceInfo, DevicePoolId, NetworkState, OperatingSystem, TestBatch, TestBatchResults},
    runtime::{CancellationSignal, Deferred, VendorFuture},
    screenshot::{RgbaFrame, Rotation, Screenshottable},
    vendor::{Device, VendorContext},
};
use std::{
    future::{Future, poll_fn},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};
type Pull = Arc<dyn Fn(String, PathBuf, VendorContext) -> VendorFuture<'static, bool> + Send + Sync>;
fn io(e: std::io::Error) -> VendorError {
    VendorError::Other(failure("java.io.IOException", &e.to_string()))
}
struct Commands {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    real: bool,
}
impl WorkerCommands for Commands {
    fn execute_worker_command<'a>(
        &'a self,
        command: Vec<String>,
        _: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(command.clone());
            if self.real {
                let output = tokio::process::Command::new(&command[0])
                    .args(&command[1..])
                    .output()
                    .await
                    .map_err(io)?;
                Ok(Some(CommandResult {
                    stdout: String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .map(str::to_owned)
                        .collect(),
                    stderr: vec![],
                    exit_code: output.status.code(),
                }))
            } else {
                Ok(Some(CommandResult {
                    stdout: vec![],
                    stderr: vec![],
                    exit_code: Some(0),
                }))
            }
        })
    }
}
struct Fake {
    manager: RemoteFileManager,
    serial: String,
    transport: Transport,
    logger: Logger,
    pull: Pull,
    pulls: Arc<Mutex<Vec<String>>>,
}
impl Device for Fake {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            operating_system: OperatingSystem { version: "17".into() },
            serial_number: self.serial.as_str().into(),
            model: "sim".into(),
            manufacturer: "Apple".into(),
            network_state: NetworkState::Connected,
            device_features: vec![],
            healthy: true,
        }
    }
    fn abi(&self) -> String {
        "arm64".into()
    }
    fn worker_id(&self) -> Option<String> {
        Some(self.transport.id())
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn prepare<'a>(&'a self, _: Arc<Configuration>, _: VendorContext) -> VendorFuture<'a, ()> {
        panic!("unused")
    }
    fn execute<'a>(
        &'a self,
        _: Arc<Configuration>,
        _: DevicePoolId,
        _: BatchRef,
        _: Arc<dyn Deferred<TestBatchResults>>,
        _: VendorContext,
    ) -> VendorFuture<'a, ()> {
        panic!("unused")
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
impl Screenshottable for Fake {
    fn orientation(&self) -> VendorResult<Rotation> {
        Ok(Rotation::Rotation0)
    }
    fn screenshot<'a>(&'a self, _: Duration, _: Arc<dyn CancellationSignal>) -> VendorFuture<'a, Option<RgbaFrame>> {
        panic!("unused")
    }
}
impl LogProducer for Fake {
    fn add_line_listener(&self, _: Arc<dyn LineListener>) -> VendorResult<()> {
        Ok(())
    }
    fn remove_line_listener(&self, _: &Arc<dyn LineListener>) -> VendorResult<()> {
        Ok(())
    }
}
macro_rules! unused {($name:ident($($arg:ident:$ty:ty),*)->$result:ty)=>{fn $name<'a>(&'a self,$($arg:$ty,)*_:&'a VendorContext)->VendorFuture<'a,$result>{$(let _=$arg;)*panic!("unused")}}}
impl AppleDevice for Fake {
    fn udid(&self) -> &str {
        "udid"
    }
    fn remote_file_manager(&self) -> &RemoteFileManager {
        &self.manager
    }
    fn storage_path(&self) -> &str {
        self.manager.remote_directory()
    }
    fn transport(&self) -> &Transport {
        &self.transport
    }
    fn sdk(&self) -> Sdk {
        Sdk::IphoneSimulator
    }
    fn binary_environment(&self) -> &AppleBinaryEnvironment {
        panic!("unused")
    }
    fn pull_folder<'a>(&'a self, src: &'a str, dst: &'a Path, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        self.pulls.lock().unwrap().push(src.into());
        (self.pull)(src.into(), dst.into(), context.clone())
    }
    unused!(setup()->());
    unused!(execute_worker_command(command: Vec<String>) -> Option<CommandResult>);
    unused!(push_file(src: &'a Path, dst: &'a str) -> bool);
    unused!(pull_file(src: &'a str, dst: &'a Path) -> bool);
    unused!(push_folder(src: &'a Path, dst: &'a str) -> bool);
    unused!(push_shared(src: &'a Path, dst: &'a str) -> bool);
    unused!(install(path: &'a str) -> bool);
    unused!(get_screenshot(timeout: Duration, dst: &'a Path) -> bool);
    unused!(start_video_recording(path: &'a str) -> Option<CommandResult>);
    unused!(stop_video_recording()->bool);
    unused!(shutdown()->bool);
    unused!(erase()->bool);
}
struct Harness {
    tmp: tempfile::TempDir,
    p: Arc<ArtifactProcessor>,
    device: Arc<Fake>,
    context: VendorContext,
    files: Arc<FileManager>,
    logging: SshLogging,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    errors: Arc<Mutex<Vec<String>>>,
}
impl Harness {
    fn new(capacity: i32, concurrent: i32, share: i32, pull: Pull, real: bool) -> VendorResult<Self> {
        let tmp = tempfile::tempdir().map_err(io)?;
        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let errors: Arc<Mutex<Vec<String>>> = Arc::default();
        let e = errors.clone();
        let logging = SshLogging::new(
            Arc::new(Mutex::new(MarathonLogging::default())),
            Arc::new(move |_, level, m, _| {
                if level == jcompat::logging::Level::Error {
                    e.lock().unwrap().push(m);
                }
            }),
        );
        let clock = Arc::new(support::Clock::default());
        let context = VendorContext {
            cancellation: Signal::new(),
            clock: clock.clone(),
            hooks: clock.clone(),
        };
        let p = ArtifactProcessor::new(
            capacity,
            concurrent,
            share,
            Arc::new(support::Runtime),
            clock,
            logging.clone(),
        )
        .unwrap();
        let storage = if real {
            tmp.path().join("worker").to_string_lossy().into_owned()
        } else {
            "/tmp/marathon/udid".into()
        };
        let device = Arc::new(Fake {
            manager: RemoteFileManager::new(
                storage,
                "udid".into(),
                Arc::new(Commands {
                    calls: calls.clone(),
                    real,
                }),
                logging.clone(),
            ),
            serial: "udid@worker".into(),
            transport: Transport::Local,
            logger: logging.logger("Fake"),
            pull,
            pulls: Arc::default(),
        });
        let files = Arc::new(FileManager::new(
            255,
            255,
            &tmp.path().join("output").to_string_lossy(),
            "/",
        ));
        Ok(Self {
            tmp,
            p,
            device,
            context,
            files,
            logging,
            calls,
            errors,
        })
    }
    fn listener(&self, id: &str) -> ResultBundleRunListener {
        self.custom(
            id,
            self.device.clone(),
            Some(self.p.clone()),
            self.context.clone(),
            configuration(),
        )
    }
    fn custom(
        &self,
        id: &str,
        device: Arc<dyn AppleDevice>,
        p: Option<Arc<ArtifactProcessor>>,
        c: VendorContext,
        config: XcresultConfiguration,
    ) -> ResultBundleRunListener {
        ResultBundleRunListener::new(
            device,
            config,
            DevicePoolId { name: "pool".into() },
            Arc::new(TestBatch {
                tests: vec![],
                id: id.into(),
            }),
            self.files.clone(),
            p,
            c,
            self.logging.clone(),
        )
    }
    fn remote(&self, id: &str) -> String {
        self.device.manager.remote_xcresult_for_id(id)
    }
    fn local(&self, id: &str) -> PathBuf {
        self.tmp
            .path()
            .join("output/device-files/pool/udid-worker/xcresult")
            .join(format!("{id}.xcresult"))
    }
    async fn close(&self) -> VendorResult<Vec<String>> {
        self.p.close_and_join(300_000, 60_000, &self.context).await
    }
}
fn configuration() -> XcresultConfiguration {
    XcresultConfiguration {
        pull: None,
        pulling_policy: PullingPolicy::Always,
        remote_clean: true,
        attachments: AttachmentsConfiguration {
            system_attachment_lifetime: Lifetime::KeepAlways,
            user_attachment_lifetime: Lifetime::KeepAlways,
        },
        preferred_screen_capture_format: None,
        queue: XcresultQueueConfiguration::default_instance(),
    }
}
fn successful() -> Pull {
    Arc::new(|src, dst, _| {
        Box::pin(async move {
            let copy = dst.join(src.rsplit('/').next().unwrap());
            std::fs::create_dir_all(&copy).map_err(io)?;
            std::fs::write(copy.join("Info.plist"), "pulled").map_err(io)?;
            Ok(true)
        })
    })
}
async fn once<T>(mut f: Pin<&mut impl Future<Output = T>>) -> Poll<T> {
    poll_fn(|cx| Poll::Ready(f.as_mut().poll(cx))).await
}
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}
#[tokio::test(start_paused = true)]
async fn failed_pulls_keep_remote_bundles_and_are_reported_at_finalization_without_poisoning_following_bundles()
-> VendorResult<()> {
    for throws in [false, true] {
        let h = Harness::new(
            4,
            4,
            1,
            Arc::new(move |src, dst, c| {
                Box::pin(async move {
                    if src.contains("failed") {
                        let copy = dst.join(src.rsplit('/').next().unwrap());
                        std::fs::create_dir_all(copy).map_err(io)?;
                        if throws {
                            Err(VendorError::Other(failure("java.io.IOException", "transport closed")))
                        } else {
                            Ok(false)
                        }
                    } else {
                        successful()(src, dst, c).await
                    }
                })
            }),
            false,
        )?;
        let failed = h.listener("failed");
        failed.before_test_run().await?;
        failed.after_test_run().await?;
        h.listener("next").after_test_run().await?;
        let retained = h.close().await?;
        assert_eq!(retained.len(), 1);
        let d = retained[0].to_owned();
        for detail in ["failed", "worker local", "udid@worker", "local copy:"] {
            assert!(d.contains(detail));
        }
        assert!(d.contains(if throws { "transport closed" } else { "pull failed" }));
        assert_eq!(h.p.kept_sources("udid@worker"), vec![h.remote("failed")]);
        assert!(h.local("next").exists());
        assert_eq!(
            h.calls.lock().unwrap().as_slice(),
            &[vec!["rm".to_owned(), "-rf".into(), h.remote("next").to_owned()]]
        );
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn failed_local_finalization_keeps_the_remote_bundle_and_never_replaces_an_existing_destination()
-> VendorResult<()> {
    for case in ["empty directory", "unrelated file", "missing pulled bundle"] {
        let h = Harness::new(
            2,
            4,
            1,
            if case == "missing pulled bundle" {
                Arc::new(|_, _, _| Box::pin(async { Ok(true) }))
            } else {
                successful()
            },
            false,
        )?;
        let local = h.local("batch");
        std::fs::create_dir_all(local.parent().unwrap()).map_err(io)?;
        match case {
            "empty directory" => std::fs::create_dir_all(&local).map_err(io)?,
            "unrelated file" => std::fs::write(&local, "unrelated").map_err(io)?,
            _ => {}
        }
        h.listener("batch").after_test_run().await?;
        let retained = h.close().await?;
        assert_eq!(retained.len(), 1);
        let diagnostic = retained[0].to_owned();
        assert!(diagnostic.contains(if case == "missing pulled bundle" {
            "NoSuchFileException"
        } else {
            "FileAlreadyExistsException"
        }));
        let oracle = include_str!("../../../fixtures/oracle/artifact-queue/events.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|row| {
                row["scenario"]
                    == "failed local finalization keeps the remote bundle and never replaces an existing destination"
                    && row["message"].as_str().is_some_and(|message| {
                        message.contains(if case == "missing pulled bundle" {
                            "NoSuchFileException"
                        } else {
                            "FileAlreadyExistsException"
                        })
                    })
            })
            .unwrap();
        let expected = oracle["message"].as_str().unwrap();
        let id = expected.split_whitespace().nth(4).unwrap();
        let expected = expected
            .replace(id, "batch")
            .replace("<tmp>", &h.tmp.path().join("output").to_string_lossy());
        assert_eq!(diagnostic, expected);
        assert!(h.calls.lock().unwrap().is_empty());
        if case == "unrelated file" {
            assert_eq!(std::fs::read_to_string(local).map_err(io)?, "unrelated");
        }
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn cancelled_unstarted_and_rejected_bundles_are_reported_once_with_recovery_details() -> VendorResult<()> {
    let entered = SharedDeferred::new();
    let e = entered.clone();
    let h = Harness::new(
        2,
        4,
        2,
        Arc::new(move |src, dst, c| {
            let e = e.clone();
            Box::pin(async move {
                std::fs::create_dir_all(dst.join(src.rsplit('/').next().unwrap())).map_err(io)?;
                e.complete(Ok(()));
                Err(VendorError::Cancelled(c.cancellation.cancelled().await))
            })
        }),
        false,
    )?;
    h.listener("in-flight").after_test_run().await?;
    h.listener("queued").after_test_run().await?;
    entered.wait().await?;
    let rejected = h.listener("rejected");
    let waiting = rejected.after_test_run();
    tokio::pin!(waiting);
    assert!(once(waiting.as_mut()).await.is_pending());
    let retained = h.p.close_and_join(1_000, 500, &h.context).await?;
    waiting.await?;
    assert_eq!(retained.len(), 3);
    for diagnostic in &retained {
        assert!(diagnostic.to_owned().contains("worker local (udid@worker)"));
    }
    assert!(retained[0].to_owned().contains("processing did not complete"));
    assert!(retained[1].to_owned().contains("local copy: none"));
    assert!(retained[2].to_owned().contains("artifact queue is closed"));
    assert_eq!(h.p.kept_sources("udid@worker").len(), 3);
    assert!(h.calls.lock().unwrap().is_empty());
    let idle = Harness::new(2, 4, 1, successful(), false)?;
    idle.listener("unstarted").after_test_run().await?;
    let d = idle.p.close_and_join(0, 500, &idle.context).await?;
    assert!(d[0].to_owned().contains("not started before shutdown"));
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn pull_that_ignores_cancellation_is_reported_and_kept_at_the_shutdown_deadline_and_not_reported_again()
-> VendorResult<()> {
    let gate = SharedDeferred::new();
    let g = gate.clone();
    let h = Harness::new(
        2,
        4,
        1,
        Arc::new(move |_, _, _| {
            let g = g.clone();
            Box::pin(async move { g.wait().await.map(|v| *v) })
        }),
        false,
    )?;
    let listener = h.listener("batch");
    listener.before_test_run().await?;
    listener.after_test_run().await?;
    settle().await;
    let retained = h.p.close_and_join(1_000, 500, &h.context).await?;
    assert_eq!(retained.len(), 1);
    assert!(
        retained[0]
            .to_owned()
            .contains("still running 500 ms after shutdown cancelled it")
    );
    assert_eq!(h.p.kept_sources("udid@worker"), vec![h.remote("batch")]);
    gate.complete(Ok(false));
    settle().await;
    assert_eq!(h.close().await?, retained);
    assert!(h.calls.lock().unwrap().is_empty());
    assert_eq!(
        h.errors
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.starts_with("xcresult bundle"))
            .count(),
        1
    );
    Ok(())
}
#[tokio::test]
async fn same_run_device_cleanup_keeps_an_undelivered_bundle_until_a_later_run() -> VendorResult<()> {
    let h = Harness::new(2, 4, 1, Arc::new(|_, _, _| Box::pin(async { Ok(false) })), true)?;
    let remote = h.remote("failed").to_owned();
    std::fs::create_dir_all(&remote).map_err(io)?;
    std::fs::write(Path::new(&remote).join("Info.plist"), "only copy").map_err(io)?;
    let stale = Path::new(h.device.storage_path()).join("stale");
    std::fs::write(&stale, "stale").map_err(io)?;
    h.listener("failed").after_test_run().await?;
    h.p.await_owner("udid@worker", &h.context).await?;
    let keep =
        h.p.kept_sources("udid@worker")
            .iter()
            .map(String::clone)
            .collect::<Vec<_>>();
    h.device.manager.remove_remote_directory(&keep, &h.context).await?;
    assert_eq!(
        std::fs::read_to_string(Path::new(&remote).join("Info.plist")).map_err(io)?,
        "only copy"
    );
    assert!(!stale.exists());
    h.close().await?;
    h.device.manager.remove_remote_directory(&[], &h.context).await?;
    assert!(!Path::new(h.device.storage_path()).exists());
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn cancelled_admission_to_a_full_queue_keeps_and_reports_the_bundle() -> VendorResult<()> {
    let gate = SharedDeferred::new();
    let g = gate.clone();
    let h = Harness::new(
        1,
        4,
        1,
        Arc::new(move |src, dst, c| {
            let g = g.clone();
            Box::pin(async move {
                g.wait_in(c.cancellation.clone()).await?;
                successful()(src, dst, c).await
            })
        }),
        false,
    )?;
    h.listener("first").after_test_run().await?;
    let signal = Signal::new();
    let mut c = h.context.clone();
    c.cancellation = signal.clone();
    let listener = h.custom("blocked", h.device.clone(), Some(h.p.clone()), c, configuration());
    let admission = listener.after_test_run();
    tokio::pin!(admission);
    assert!(once(admission.as_mut()).await.is_pending());
    signal.cancel(requested("device disposed"));
    assert!(matches!(admission.await, Err(VendorError::Cancelled(_))));
    assert_eq!(h.p.kept_sources("udid@worker"), vec![h.remote("blocked")]);
    h.device
        .manager
        .remove_remote_directory(&[h.remote("blocked").to_owned()], &h.context)
        .await?;
    assert_eq!(
        h.calls.lock().unwrap()[0],
        vec![
            "find",
            "/tmp/marathon/udid",
            "-mindepth",
            "1",
            "-maxdepth",
            "1",
            "!",
            "-name",
            "udid.blocked.xcresult",
            "-exec",
            "rm",
            "-rf",
            "{}",
            "+"
        ]
    );
    gate.complete(Ok(()));
    let retained = h.close().await?;
    assert_eq!(retained.len(), 1);
    assert!(retained[0].to_owned().contains("admission was cancelled"));
    assert_eq!(h.device.pulls.lock().unwrap().len(), 1);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn healthy_worker_hands_over_its_bundle_while_a_stalled_worker_holds_its_share_and_waiting_bundles_stay_protected()
-> VendorResult<()> {
    let gate = SharedDeferred::new();
    let g = gate.clone();
    let h = Harness::new(
        8,
        4,
        2,
        Arc::new(move |src, dst, c| {
            let g = g.clone();
            Box::pin(async move {
                g.wait_in(c.cancellation.clone()).await?;
                successful()(src, dst, c).await
            })
        }),
        false,
    )?;
    for i in 0..2 {
        let listener = h.listener(&format!("slow-{i}"));
        listener.before_test_run().await?;
        listener.after_test_run().await?;
    }
    let third = h.listener("slow-2");
    third.before_test_run().await?;
    let waiting = third.after_test_run();
    tokio::pin!(waiting);
    assert!(once(waiting.as_mut()).await.is_pending());
    let fast = Arc::new(Fake {
        manager: RemoteFileManager::new(
            "/tmp/marathon/fast".into(),
            "fast".into(),
            Arc::new(Commands {
                calls: h.calls.clone(),
                real: false,
            }),
            h.logging.clone(),
        ),
        serial: "fast".into(),
        transport: Transport::Ssh {
            addr: "fast".into(),
            port: 22,
            authentication: None,
            check_reachability: false,
        },
        logger: h.logging.logger("Fake"),
        pull: successful(),
        pulls: Arc::default(),
    });
    let listener = h.custom("healthy", fast, Some(h.p.clone()), h.context.clone(), configuration());
    listener.before_test_run().await?;
    listener.after_test_run().await?;
    h.p.await_owner("fast", &h.context).await?;
    assert_eq!(h.p.kept_sources("udid@worker").len(), 3);
    assert!(h.p.kept_sources("fast").is_empty());
    assert_eq!(h.calls.lock().unwrap().len(), 1);
    gate.complete(Ok(()));
    waiting.await?;
    assert!(h.close().await?.is_empty());
    assert_eq!(h.calls.lock().unwrap().len(), 4);
    Ok(())
}
#[test]
fn production_bridge_delivers_renames_and_cleans_before_provider_report() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let vendor = core.vendor();
            let clock = core.vendor_clock();
            let hooks = marathon_core::executor::hooks::RuntimeHooks::live();
            let cancellation = Signal::new();
            let context = VendorContext {
                cancellation: cancellation.clone(),
                clock,
                hooks: hooks.clone(),
            };
            call_vendor(vendor.clone().as_ref(), cancellation, move |_| {
                Box::pin(async move {
                    let mut h = Harness::new(2, 4, 1, successful(), false)?;
                    h.p = ArtifactProcessor::new(2, 4, 1, vendor.clone(), hooks, h.logging.clone()).unwrap();
                    h.context = context;
                    h.listener("bridge").before_test_run().await?;
                    h.listener("bridge").after_test_run().await?;
                    h.p.await_owner("udid@worker", &h.context).await?;
                    assert!(h.local("bridge").exists());
                    assert!(h.close().await?.is_empty());
                    assert_eq!(
                        h.calls.lock().unwrap().as_slice(),
                        &[vec![
                            "rm".to_owned(),
                            "-rf".into(),
                            "/tmp/marathon/udid/udid.bridge.xcresult".into()
                        ]]
                    );
                    Ok(())
                })
            })
            .await
            .map_err(|e| marathon_core::error::OperationError::Failed(e.failure().clone()))
        })
    })
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn macos_inline_path_obeys_policy_and_finishes_delivery_before_returning() -> VendorResult<()> {
    for policy in [PullingPolicy::Always, PullingPolicy::Never, PullingPolicy::OnFailure] {
        for failed in [false, true] {
            for clean in [false, true] {
                let h = Harness::new(2, 4, 1, successful(), false)?;
                let mut config = configuration();
                config.pulling_policy = policy;
                config.remote_clean = clean;
                let fm_logger = h.logging.registry.lock().unwrap().logger(
                    "com.malinskiy.marathon.io.FileManager",
                    Some(jcompat::logging::Level::Warn),
                );
                let listener = h.custom("inline", h.device.clone(), None, h.context.clone(), config);
                listener.before_test_run().await?;
                if failed {
                    listener
                        .test_run_failed(
                            "failed",
                            marathon_apple::logparser::parser::DeviceFailureReason::Unknown,
                        )
                        .await?;
                }
                listener.after_test_run().await?;
                assert!(!h.logging.enabled(&fm_logger, jcompat::logging::Level::Debug));
                let pull = policy == PullingPolicy::Always || (policy == PullingPolicy::OnFailure && failed);
                assert_eq!(h.local("inline").exists(), pull);
                assert_eq!(h.device.pulls.lock().unwrap().len(), usize::from(pull));
                assert_eq!(h.calls.lock().unwrap().len(), usize::from(clean));
            }
        }
    }
    Ok(())
}
