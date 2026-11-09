//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/SkipCopyingDerivedDataTest.kt (bundle construction).
#![allow(dead_code)]
use crate::{
    support,
    test_root_device::{Fake, TestResult, v},
};
use jcompat::jtime::instant_of;
use marathon_apple::{
    apple_device_registry::AppleDeviceRegistry, apple_test_bundle_identifier::AppleTestBundleIdentifier,
    model::AppleTestBundle, nm_test_parser::NmTestParser,
};
use marathon_config::{
    VendorConfiguration,
    bind_vendor::bind_vendor_configuration,
    binder::Binder,
    binding::{BindingContext, Profile},
    node,
    vendor::AppleTestBundleConfiguration,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::{path::Path, sync::Arc};
/// Plainly not a library: a test must never need the real third-party file (D20).
pub const STAND_IN_LIBRARY: &[u8] = b"fake stand-in for libxctest-parser.dylib, written by a test\n";
/// A supplied directory in the Kotlin resource layout, under `root`.
pub fn parser_libraries(root: &Path) -> TestResult<marathon_apple::xctest_parser::ParserLibraries> {
    let directory = root.join("supplied-parser");
    for platform in [
        "iPhoneSimulator",
        "MacOSX",
        "AppleTVSimulator",
        "WatchSimulator",
        "XRSimulator",
    ] {
        std::fs::create_dir_all(directory.join(platform))?;
        std::fs::write(
            directory.join(platform).join("libxctest-parser.dylib"),
            STAND_IN_LIBRARY,
        )?;
    }
    Ok(marathon_apple::xctest_parser::ParserLibraries::new(directory))
}
pub fn input(reuse: bool, skip: bool) -> Value {
    json!({"id":"rust-parsing","sdk":"IPHONESIMULATOR","type":"XCUITEST","app":"CloudApp.app","runner":"UITests-Runner.app","test":"UITests-Runner.app/PlugIns/UITests.xctest","reuse":reuse,"skip":skip,"installer":true,"udid":"udid"})
}
pub fn configuration(c: &Value, root: &Path) -> TestResult<VendorConfiguration> {
    let ctx = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/Marathonfile"),
        marathonfile_dir: PathBuf::from("/"),
        process_dir: PathBuf::from("/"),
        environment: vec![],
        now: instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    };
    let stream = node::load("type: iOS");
    let binder = Binder::new(&ctx, &stream);
    let mut vendor = v(binder.first(&stream, bind_vendor_configuration))?.flatten().unwrap();
    let path = |name: &str| PathBuf::from(root.join(c[name].as_str().unwrap()).to_string_lossy().into_owned());
    let mut bundle =
        AppleTestBundleConfiguration::unresolved(Some(path("app")), Some(path("runner")), None, None, None);
    bundle.app = Some(path("app"));
    bundle.test_app = c["reuse"].as_bool().unwrap().then(|| path("runner"));
    bundle.xctest = Some(path("test"));
    if let VendorConfiguration::Ios(ios) = &mut vendor {
        ios.bundle = Some(bundle);
        ios.skip_copying_derived_data = c["skip"].as_bool().unwrap();
    }
    Ok(vendor)
}
pub async fn bundle(device: &Fake, c: &Value) -> TestResult<Arc<AppleTestBundle>> {
    Ok(Arc::new(v(AppleTestBundle::new(
        Some(device.root.join(c["app"].as_str().unwrap())),
        c["reuse"]
            .as_bool()
            .unwrap()
            .then(|| device.root.join(c["runner"].as_str().unwrap())),
        device.root.join(c["test"].as_str().unwrap()),
        crate::test_root_device::sdk(c),
        Arc::new(support::Runtime),
        device.logging.clone(),
    )
    .await)?))
}
pub fn nm(device: &Arc<Fake>, config: VendorConfiguration) -> (NmTestParser, marathon_core::vendor::DeviceRef) {
    let registry = Arc::new(AppleDeviceRegistry::default());
    let identity = registry.register(device.clone());
    let parser = NmTestParser::new(
        Arc::new(config),
        registry,
        Arc::new(AppleTestBundleIdentifier::default()),
        vec![],
        Arc::new(support::Runtime),
        device.logging.clone(),
    );
    (parser, identity)
}
