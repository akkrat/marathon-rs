//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/AppleDeviceTestRunnerTest.kt (recording device).
#![allow(dead_code)]
use crate::{support, test_root_device::Fake};
use jcompat::{jtime::Duration, logging::Logger};
use marathon_apple::{
    apple_device::AppleDevice,
    bin::AppleBinaryEnvironment,
    cmd::{BaseCommand, CommandSession, SessionControl, failure, remote::ssh::sshj::config::SshLogging},
    execute_test_request::LineListeners,
    model::Sdk,
    remote_file_manager::RemoteFileManager,
    test::{TestEvent, TestRequest},
    xcodebuild_session::{EventReceiver, XcodebuildSession},
};
use marathon_config::{Configuration, devices::Transport};
use marathon_core::{
    error::{VendorError, VendorResult},
    listener::{LineListener, LogProducer},
    model::{BatchRef, DeviceInfo, DevicePoolId, TestBatchResults},
    runtime::{CancellationSignal, Deferred, VendorFuture},
    screenshot::{RgbaFrame, Rotation, Screenshottable},
    vendor::{Device, VendorContext},
};
use marathon_error::FailureKind;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;
#[derive(Default)]
pub struct Plan {
    pub events: Vec<Vec<TestEvent>>,
    pub output_failure: Option<VendorError>,
    pub closing_failure: Option<VendorError>,
    pub wait_close: bool,
    pub hang_output: bool,
}
pub struct RunnerDevice {
    pub inner: Fake,
    pub dylib_push_succeeds: bool,
    pub staged: Mutex<Vec<std::path::PathBuf>>,
    pub requests: Mutex<Vec<TestRequest>>,
    pub plan: Mutex<Plan>,
    pub session: Mutex<Option<Arc<XcodebuildSession>>>,
    pub release: Arc<Notify>,
    pub closing: Arc<Notify>,
    pub closed: Arc<Notify>,
    pub listeners: Arc<LineListeners>,
    pub logging: SshLogging,
    pub calls: Arc<Mutex<Vec<String>>>,
}
impl RunnerDevice {
    pub fn new(inner: Fake, logging: SshLogging) -> Self {
        Self {
            inner,
            dylib_push_succeeds: true,
            staged: Mutex::default(),
            requests: Mutex::default(),
            plan: Mutex::default(),
            session: Mutex::default(),
            release: Arc::default(),
            closing: Arc::default(),
            closed: Arc::default(),
            listeners: Arc::default(),
            logging,
            calls: Arc::default(),
        }
    }
}
impl Device for RunnerDevice {
    fn info(&self) -> DeviceInfo {
        self.inner.info()
    }
    fn abi(&self) -> String {
        self.inner.abi()
    }
    fn worker_id(&self) -> Option<String> {
        self.inner.worker_id()
    }
    fn logger(&self) -> &Logger {
        self.inner.logger()
    }
    fn prepare<'a>(&'a self, c: Arc<Configuration>, ctx: VendorContext) -> VendorFuture<'a, ()> {
        self.inner.prepare(c, ctx)
    }
    fn execute<'a>(
        &'a self,
        c: Arc<Configuration>,
        p: DevicePoolId,
        b: BatchRef,
        r: Arc<dyn Deferred<TestBatchResults>>,
        ctx: VendorContext,
    ) -> VendorFuture<'a, ()> {
        self.inner.execute(c, p, b, r, ctx)
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let s = self.session.lock().unwrap().clone();
            if let Some(s) = s {
                self.release.notify_one();
                s.join_producer().await?;
            }
            self.inner.dispose().await
        })
    }
}
impl LogProducer for RunnerDevice {
    fn add_line_listener(&self, l: Arc<dyn LineListener>) -> VendorResult<()> {
        self.listeners.add(l);
        self.calls.lock().unwrap().push("add listener".into());
        Ok(())
    }
    fn remove_line_listener(&self, l: &Arc<dyn LineListener>) -> VendorResult<()> {
        self.listeners.remove(l);
        self.calls.lock().unwrap().push("remove listener".into());
        Ok(())
    }
}
impl Screenshottable for RunnerDevice {
    fn orientation(&self) -> VendorResult<Rotation> {
        self.inner.orientation()
    }
    fn screenshot<'a>(
        &'a self,
        timeout: Duration,
        signal: Arc<dyn CancellationSignal>,
    ) -> VendorFuture<'a, Option<RgbaFrame>> {
        self.inner.screenshot(timeout, signal)
    }
}
macro_rules! forward {($($name:ident($($arg:ident:$ty:ty),*) -> $ret:ty;)*)=>{$(fn $name<'a>(&'a self,$($arg:$ty,)*ctx:&'a VendorContext)->VendorFuture<'a,$ret>{self.inner.$name($($arg,)*ctx)})*};}
impl AppleDevice for RunnerDevice {
    fn udid(&self) -> &str {
        self.inner.udid()
    }
    fn remote_file_manager(&self) -> &RemoteFileManager {
        self.inner.remote_file_manager()
    }
    fn storage_path(&self) -> &str {
        self.inner.storage_path()
    }
    fn transport(&self) -> &Transport {
        self.inner.transport()
    }
    fn sdk(&self) -> Sdk {
        self.inner.sdk()
    }
    fn binary_environment(&self) -> &AppleBinaryEnvironment {
        self.inner.binary_environment()
    }
    forward! {setup()->();execute_worker_command(command:Vec<String>)->Option<marathon_apple::cmd::CommandResult>;pull_file(src:&'a str,dst:&'a Path)->bool;push_folder(src:&'a Path,dst:&'a str)->bool;pull_folder(src:&'a str,dst:&'a Path)->bool;push_shared(src:&'a Path,dst:&'a str)->bool;install(path:&'a str)->bool;get_screenshot(timeout:Duration,dst:&'a Path)->bool;start_video_recording(path:&'a str)->Option<marathon_apple::cmd::CommandResult>;stop_video_recording()->bool;shutdown()->bool;erase()->bool;}
    fn push_file<'a>(&'a self, src: &'a Path, dst: &'a str, ctx: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            if dst.ends_with("libxctest-parser.dylib") {
                self.staged.lock().unwrap().push(src.to_path_buf());
                assert!(tokio::fs::metadata(src).await.is_ok());
                if !self.dylib_push_succeeds {
                    return Ok(false);
                }
            }
            self.inner.push_file(src, dst, ctx).await
        })
    }
    fn execute_test_request<'a>(
        &'a self,
        request: TestRequest,
        session: Arc<XcodebuildSession>,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, EventReceiver> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            if let Some(previous) = self.session.lock().unwrap().as_ref() {
                previous.check_closed(&self.info().serial_number.to_owned())?;
            }
            *self.session.lock().unwrap() = Some(session.clone());
            let plan = std::mem::take(&mut *self.plan.lock().unwrap());
            let release = self.release.clone();
            let closing = self.closing.clone();
            let closed = self.closed.clone();
            let calls = self.calls.clone();
            let command = CommandSession::new(
                BaseCommand::new(1, Arc::new(support::Runtime)),
                SessionControl {
                    is_alive: Box::new(|| true),
                    terminate: Box::new(|| Ok(())),
                    kill: Box::new(|| Ok(())),
                    close: Box::new(move || {
                        let release = release.clone();
                        let closing = closing.clone();
                        let closed = closed.clone();
                        let calls = calls.clone();
                        let error = plan.closing_failure.clone();
                        Box::pin(async move {
                            closing.notify_one();
                            if plan.wait_close {
                                release.notified().await;
                            }
                            if let Some(error) = error {
                                return Err(error);
                            }
                            calls.lock().unwrap().push("closed".into());
                            closed.notify_one();
                            Ok(())
                        })
                    }),
                },
            );
            Ok(session.produce(
                context.clone(),
                move |_| async move { Ok(command) },
                move |_, sender, ctx| async move {
                    for events in plan.events {
                        sender.send(events).await?;
                    }
                    if plan.hang_output {
                        return Err(VendorError::Cancelled(ctx.cancellation.cancelled().await));
                    }
                    if let Some(error) = plan.output_failure {
                        return Err(error);
                    }
                    Ok(())
                },
            ))
        })
    }
}
pub fn io(message: &str) -> VendorError {
    VendorError::Other(failure(FailureKind::Io, message))
}
