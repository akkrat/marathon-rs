//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleDevice.kt

use crate::{
    bin::AppleBinaryEnvironment,
    cmd::CommandResult,
    model::{Arch, Sdk},
    remote_file_manager::RemoteFileManager,
};
use jcompat::jtime::Duration;
use marathon_config::devices::Transport;
use marathon_core::{
    listener::LogProducer,
    runtime::VendorFuture,
    screenshot::Screenshottable,
    vendor::{Device, VendorContext},
};
use std::path::Path;

pub const SHARED_PATH: &str = "/tmp/marathon";

pub fn apple_worker_id(transport: &Transport) -> Option<String> {
    Some(transport.id())
}

/// Recompute after simulator setup: setup can change the ABI. Other SDKs retain
/// Kotlin's UDID heuristic, including macOS and non-iOS simulators (A45).
pub fn apple_arch(sdk: Sdk, abi: &String, udid: &str) -> Arch {
    if sdk == Sdk::IphoneSimulator {
        if *abi == String::from("x86_64") {
            Arch::x86_64
        } else {
            Arch::arm64
        }
    } else if udid.contains('-') {
        Arch::arm64e
    } else {
        Arch::arm64
    }
}

pub fn storage_path_for(udid: &str) -> String {
    format!("{SHARED_PATH}/{udid}")
}

pub trait AppleDevice: Device + Screenshottable + LogProducer {
    /// Concrete devices check and replace lastXcodebuildSession before using the shared body.
    fn execute_test_request<'a>(
        &'a self,
        _request: crate::test::TestRequest,
        _session: std::sync::Arc<crate::xcodebuild_session::XcodebuildSession>,
        _context: &'a VendorContext,
    ) -> VendorFuture<'a, crate::xcodebuild_session::EventReceiver> {
        Box::pin(async {
            Err(marathon_core::error::VendorError::Other(crate::cmd::failure(
                "java.lang.UnsupportedOperationException",
                "executeTestRequest is not implemented",
            )))
        })
    }
    fn udid(&self) -> &str;
    fn remote_file_manager(&self) -> &RemoteFileManager;
    fn storage_path(&self) -> &str;
    fn transport(&self) -> &Transport;
    fn sdk(&self) -> Sdk;
    fn binary_environment(&self) -> &AppleBinaryEnvironment;
    fn arch(&self) -> Arch {
        apple_arch(self.sdk(), &self.abi(), self.udid())
    }
    /// Setup implementations classify failures as VendorError::DeviceSetup.
    fn setup<'a>(&'a self, context: &'a VendorContext) -> VendorFuture<'a, ()>;
    fn execute_worker_command<'a>(
        &'a self,
        command: Vec<String>,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>>;
    fn push_file<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool>;
    fn pull_file<'a>(&'a self, src: &'a str, dst: &'a Path, context: &'a VendorContext) -> VendorFuture<'a, bool>;
    fn push_generated_file<'a>(
        &'a self,
        src: &'a Path,
        dst: &'a str,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, bool> {
        self.push_file(src, dst, context)
    }
    fn push_folder<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool>;
    fn pull_folder<'a>(&'a self, src: &'a str, dst: &'a Path, context: &'a VendorContext) -> VendorFuture<'a, bool>;
    fn push_shared<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool>;
    fn install<'a>(&'a self, remote_path: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool>;
    fn get_screenshot<'a>(
        &'a self,
        timeout: Duration,
        dst: &'a Path,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, bool>;
    fn start_video_recording<'a>(
        &'a self,
        remote_path: &'a str,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>>;
    fn stop_video_recording<'a>(&'a self, context: &'a VendorContext) -> VendorFuture<'a, bool>;
    fn shutdown<'a>(&'a self, context: &'a VendorContext) -> VendorFuture<'a, bool>;
    fn erase<'a>(&'a self, context: &'a VendorContext) -> VendorFuture<'a, bool>;
}
