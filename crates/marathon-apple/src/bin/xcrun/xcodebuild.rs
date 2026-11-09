//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcodebuild/Xcodebuild.kt

use super::super::{Tools, argv};
use crate::{
    bin::java_duration_of_millis,
    cmd::{CommandSession, failure},
    model::{Sdk, XcodeVersion},
    test::TestRequest,
};
use marathon_logging::{Level, Component};
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use std::collections::BTreeMap;

pub const COLLECT_TEST_DIAGNOSTICS: &str = "-collect-test-diagnostics";
/// The first Xcode whose xcodebuild knows [`COLLECT_TEST_DIAGNOSTICS`]. Xcode collects those diagnostics
/// with `simctl diagnose` after a simulator session in which a test recorded an issue; that writes nothing
/// while it runs and can outlast the output idle timeout on a loaded worker.
pub const COLLECT_TEST_DIAGNOSTICS_SINCE: XcodeVersion = XcodeVersion::new(14, 0, 0);

/// `LinkedHashMap.put`: replace in place, otherwise append.
fn put(map: &mut Vec<(String, String)>, key: &str, value: String) {
    match map.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = value,
        None => map.push((key.to_owned(), value)),
    }
}

/// `xcodebuild test-without-building` arguments on a simulator: `user_args` and, from
/// [`COLLECT_TEST_DIAGNOSTICS_SINCE`] on, a leading `-collect-test-diagnostics never` that a user value
/// replaces in place. An unknown version adds nothing. Maps are ordered `(key, value)` lists.
pub fn simulator_test_args(
    user_args: &[(String, String)],
    xcode_version: Option<XcodeVersion>,
) -> Vec<(String, String)> {
    match xcode_version {
        Some(version) if version >= COLLECT_TEST_DIAGNOSTICS_SINCE => {
            let mut args = vec![(COLLECT_TEST_DIAGNOSTICS.to_owned(), "never".to_owned())];
            for (key, value) in user_args {
                put(&mut args, key, value.clone());
            }
            args
        }
        _ => user_args.to_vec(),
    }
}

pub struct Xcodebuild {
    tools: Tools,
    test_batch_timeout_millis: i64,
    test_output_timeout_millis: i64,
    logger: Component,
}
impl Xcodebuild {
    pub fn new(tools: Tools, test_batch_timeout_millis: i64, test_output_timeout_millis: i64) -> Self {
        let logger = tools.component("marathon_apple::bin::xcrun::xcodebuild");
        Self {
            tools,
            test_batch_timeout_millis,
            test_output_timeout_millis,
            logger,
        }
    }
    /// Returns the live session; consuming its streams and closing it is the caller's.
    pub async fn test_without_building(
        &self,
        udid: &str,
        sdk: Sdk,
        request: &TestRequest,
        xcodebuild_test_args: &[(String, String)],
        context: &VendorContext,
    ) -> VendorResult<CommandSession> {
        let mut args = xcodebuild_test_args.to_vec();
        put(
            &mut args,
            "-enableCodeCoverage",
            if request.coverage { "YES" } else { "NO" }.into(),
        );
        if let Some(xcresult) = &request.xcresult {
            put(&mut args, "-resultBundlePath", xcresult.clone());
        }
        put(
            &mut args,
            "-destination-timeout",
            self.tools.timeouts.test_destination.seconds().to_string(),
        );
        put(
            &mut args,
            "-destination",
            format!("'platform={},id={udid}'", sdk.destination()),
        );
        let mut command = argv(["xcrun", "xcodebuild", "test-without-building", "-xctestrun"]);
        command.push(request.remote_xctestrun.clone());
        command.extend(request.to_xcodebuild_test_filter());
        for (key, value) in args.into_iter().filter(|(key, _)| key != "-xctestrun") {
            command.push(key);
            if !value.is_empty() {
                command.push(value);
            }
        }
        self.tools.log(
            &self.logger,
            Level::DEBUG,
            || format!("Running {}", command.join(" ")),
            None,
        );
        // A local executor sets this in the process's environment, and SSH puts it in the exec string (D32).
        let request = self.tools.request_with(
            command,
            java_duration_of_millis(self.test_batch_timeout_millis),
            java_duration_of_millis(self.test_output_timeout_millis),
            BTreeMap::from([("NSUnbufferedIO".to_owned(), "YES".to_owned())]),
            Some(request.workdir.clone()),
        );
        self.tools.executor.execute(request, context.clone()).await
    }
    /// The last space-separated token of the first stdout line (`Xcode 14.2`, then `Build version 14C18`).
    pub async fn get_version(&self, context: &VendorContext) -> VendorResult<XcodeVersion> {
        let output = self
            .tools
            .critical(
                argv(["xcrun", "xcodebuild", "-version"]),
                self.tools.timeouts.shell,
                context,
            )
            .await?
            .stdout;
        let first = output
            .first()
            .ok_or_else(|| VendorError::Other(failure(FailureKind::NoSuchElement, "List is empty.")))?;
        // split(" ") always has a last element.
        let version = first.rsplit(' ').next().unwrap_or_default();
        XcodeVersion::from(version)
    }
}
