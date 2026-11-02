//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/model/AndroidTestBundle.kt,
//! model/TestIdentifier.kt, model/AndroidTestStatus.kt, AndroidTestBundleIdentifier.kt,
//! extension/Test.kt and extension/ConfigurationExtensions.kt.
use crate::{dex_test_parser::DexTestBundleFiles, support::*};
use jcompat::{
    file_manager::{JavaPath, absolute_file_path_utf16},
    writers::JavaString,
};
use marathon_config::vendor::*;
use marathon_core::{
    error::*,
    model::{DeviceFeature, MetaProperty, Test, TestStatus},
    vendor::{BundleRef, TestBundle, TestBundleIdentifier},
};
use marathon_dex::apk_parser::InstrumentationInfo;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub struct AndroidTestBundle {
    pub files: DexTestBundleFiles,
    id: JavaString,
    info: Mutex<Option<InstrumentationInfo>>,
}
impl AndroidTestBundle {
    pub fn new(files: DexTestBundleFiles, cwd: &JavaPath) -> Self {
        let id = JavaString(absolute_file_path_utf16(&files.test_application.0, &cwd.0).0);
        Self {
            files,
            id,
            info: Mutex::new(None),
        }
    }
    /// Called on a vendor blocking worker. Kotlin's lazy retries after a parse failure.
    pub fn instrumentation_info(&self) -> VendorResult<InstrumentationInfo> {
        let mut info = lock(&self.info);
        if let Some(info) = info.as_ref() {
            return Ok(info.clone());
        }
        let parsed = marathon_dex::apk_parser::parse_instrumentation_info(
            &self.files.test_application.to_file_path_buf().map_err(io)?,
        )
        .map_err(dex_error)?;
        *info = Some(parsed.clone());
        Ok(parsed)
    }
}
impl TestBundle for AndroidTestBundle {
    fn id(&self) -> &JavaString {
        &self.id
    }
}
#[derive(Default)]
pub struct AndroidTestBundleIdentifier {
    bundles: Mutex<HashMap<Test, Arc<AndroidTestBundle>>>,
}
impl AndroidTestBundleIdentifier {
    pub fn put(&self, test: &Test, bundle: &Arc<AndroidTestBundle>) {
        lock(&self.bundles).insert(test.clone(), bundle.clone());
    }
    pub fn android_bundle(&self, test: &Test) -> VendorResult<Arc<AndroidTestBundle>> {
        lock(&self.bundles).get(test).cloned().ok_or_else(|| {
            let mut message = JavaString::from("Invalid test ");
            message.append(test.to_test_name('.', '#'));
            message.push_str(": can't locate test bundle");
            other(CONFIGURATION_EXCEPTION_CLASS, message)
        })
    }
}
impl TestBundleIdentifier for AndroidTestBundleIdentifier {
    fn identify(&self, test: &Test) -> VendorResult<Option<BundleRef>> {
        Ok(Some(BundleRef(self.android_bundle(test)?)))
    }
}
pub fn test_bundles_compat(c: &AndroidConfiguration) -> Vec<DexTestBundleFiles> {
    let mut files: Vec<_> = c.outputs.iter().flatten().map(to_android_test_bundle).collect();
    if let Some(test_application) = &c.test_application_output {
        files.push(DexTestBundleFiles {
            application: c.application_output.clone(),
            test_application: test_application.clone(),
            extra_applications: c.extra_applications_output.clone().unwrap_or_default(),
            split_apks: c.split_apks.clone().unwrap_or_default(),
        });
    }
    files
}
pub fn to_android_test_bundle(c: &AndroidTestBundleConfiguration) -> DexTestBundleFiles {
    DexTestBundleFiles {
        application: c.application.clone(),
        test_application: c.test_application.clone(),
        extra_applications: c.extra_applications.clone().unwrap_or_default(),
        split_apks: c.split_apks.clone().unwrap_or_default(),
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellCommandResult {
    pub output: JavaString,
    pub exit_code: i32,
}
impl From<marathon_adb::client::shell::v1::ShellCommandResult> for ShellCommandResult {
    fn from(r: marathon_adb::client::shell::v1::ShellCommandResult) -> Self {
        Self {
            output: r.output().as_str().into(),
            exit_code: r.exit_code,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TestIdentifier {
    pub class_name: JavaString,
    pub test_name: JavaString,
}
impl TestIdentifier {
    pub fn to_test(&self, meta_properties: Vec<MetaProperty>) -> Test {
        let dot = self.class_name.0.iter().rposition(|u| *u == 46);
        let (pkg, clazz) = match dot {
            Some(i) => (
                JavaString(self.class_name.0[..i].to_vec()),
                JavaString(self.class_name.0[i + 1..].to_vec()),
            ),
            None => (JavaString::default(), self.class_name.clone()),
        };
        Test {
            pkg,
            clazz,
            method: self.test_name.clone(),
            meta_properties,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AndroidTestStatus {
    Failure,
    Passed,
    Incomplete,
    AssumptionFailure,
    Ignored,
}
impl AndroidTestStatus {
    pub fn to_marathon_status(self) -> TestStatus {
        match self {
            Self::Failure => TestStatus::Failure,
            Self::Passed => TestStatus::Passed,
            Self::Incomplete => TestStatus::Incomplete,
            Self::AssumptionFailure => TestStatus::AssumptionFailure,
            Self::Ignored => TestStatus::Ignored,
        }
    }
}
pub fn is_ignored(test: &Test) -> bool {
    test.meta_properties.iter().any(|p| {
        [
            "org.junit.Ignore",
            "android.support.test.filters.Suppress",
            "android.test.suitebuilder.annotation.Suppress",
        ]
        .iter()
        .any(|s| p.name == (*s).into())
    })
}
pub fn effective_file_sync(c: &AndroidConfiguration, coverage: bool) -> FileSyncConfiguration {
    let mut result = FileSyncConfiguration {
        pull: Vec::new(),
        push: c.file_sync_configuration.push.clone(),
    };
    let mut add = |entry| {
        if !result.pull.contains(&entry) {
            result.pull.push(entry)
        }
    };
    for entry in &c.file_sync_configuration.pull {
        add(entry.clone());
    }
    if c.allure_configuration.enabled {
        add(FileSyncEntry {
            relative_path: c.allure_configuration.relative_results_directory.clone(),
            path_root: c.allure_configuration.path_root,
            aggregation_mode: AggregationMode::TestRun,
        });
    }
    if coverage {
        add(FileSyncEntry {
            relative_path: "coverage".into(),
            path_root: PathRoot::AppData,
            aggregation_mode: AggregationMode::Pool,
        });
    }
    result
}
pub fn select_recorder_type(features: &[DeviceFeature], c: &AndroidScreenRecordConfiguration) -> Option<DeviceFeature> {
    let enabled = |f| match f {
        DeviceFeature::Video => c.video_configuration.enabled,
        DeviceFeature::Screenshot => c.screenshot_configuration.enabled,
    };
    let preferred = c.preferable_recorder_type.map(|r| match r {
        AndroidRecorderType::Video => DeviceFeature::Video,
        AndroidRecorderType::Screenshot => DeviceFeature::Screenshot,
    });
    preferred.filter(|f| features.contains(f) && enabled(*f)).or_else(|| {
        [DeviceFeature::Video, DeviceFeature::Screenshot]
            .into_iter()
            .find(|f| features.contains(f) && enabled(*f))
    })
}
pub fn to_screen_recorder_command(c: &AndroidVideoConfiguration, path: &JavaString, infinite: bool) -> JavaString {
    let mut result = JavaString::from("screenrecord ");
    if c.width > 0 && c.height > 0 {
        result.push_str(&format!("--size {}x{} ", c.width, c.height));
    }
    if c.bitrate_mbps > 0 {
        result.push_str(&format!(
            "--bit-rate {} ",
            c.bitrate_mbps.wrapping_mul(1_000_000).min(200_000_000)
        ));
    }
    if c.time_limit > 0 {
        let seconds = match c.time_limit_units {
            TimeUnit::Nanoseconds => c.time_limit / 1_000_000_000,
            TimeUnit::Microseconds => c.time_limit / 1_000_000,
            TimeUnit::Milliseconds => c.time_limit / 1000,
            TimeUnit::Seconds => c.time_limit,
            TimeUnit::Minutes => c.time_limit.saturating_mul(60),
            TimeUnit::Hours => c.time_limit.saturating_mul(3600),
            TimeUnit::Days => c.time_limit.saturating_mul(86400),
        };
        result.push_str(&format!(
            "--time-limit {} ",
            if infinite { seconds } else { seconds.min(180) }
        ));
    }
    result.append(path.clone());
    result
}
