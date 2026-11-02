//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/xctestrun/v1/XctestrunTest.kt

use std::collections::BTreeSet;
use std::path::PathBuf;

use marathon_apple::plist::PropertyList;
use marathon_apple::xctestrun::v1::Xctestrun;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vendor-apple-base/fixtures/xctestrun")
        .join(name)
}

fn ordered(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn as_set(map: Vec<(String, String)>) -> BTreeSet<(String, String)> {
    map.into_iter().collect()
}

#[test]
fn test_read() {
    let xctestrun =
        Xctestrun::from_path(&fixture("UITesting_iphonesimulator11.2-x86_64.xctestrun")).expect("read fixture");
    let targets = xctestrun.targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].0, "sample-appUITests");
    let target = &targets[0].1;
    assert!(target.is_ui_test_bundle().expect("ui test bundle"));
    assert_eq!(
        target.system_attachment_lifetime().expect("system attachment"),
        "deleteOnSuccess"
    );
    assert_eq!(
        target.test_bundle_path().expect("bundle path"),
        "__TESTHOST__/PlugIns/sample-appUITests.xctest"
    );
    assert_eq!(
        target.test_host_path().expect("host path"),
        "__TESTROOT__/Debug-iphonesimulator/sample-appUITests-Runner.app"
    );
    assert_eq!(
        target.ui_target_app_path().expect("target app"),
        "__TESTROOT__/Debug-iphonesimulator/sample-app.app"
    );
    assert_eq!(
        target.user_attachment_lifetime().expect("user attachment"),
        "deleteOnSuccess"
    );
    assert_eq!(
        target
            .dependent_product_paths()
            .expect("dependent paths")
            .iter()
            .collect::<BTreeSet<_>>(),
        [
            "__TESTROOT__/Debug-iphonesimulator/sample-appUITests-Runner.app/PlugIns/sample-appUITests.xctest"
                .to_owned(),
            "__TESTROOT__/Debug-iphonesimulator/sample-app.app".to_owned(),
        ]
        .iter()
        .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        as_set(target.testing_environment_variables().expect("testing env")),
        as_set(ordered(&[
            (
                "DYLD_FRAMEWORK_PATH",
                "__TESTROOT__/Debug-iphonesimulator:__PLATFORMS__/iPhoneSimulator.platform/Developer/Library/Frameworks:/Library/Developer/CoreSimulator/Profiles/Runtimes/iOS 11.1.simruntime/Contents/Resources/RuntimeRoot/Developer/Library/PrivateFrameworks"
            ),
            (
                "DYLD_INSERT_LIBRARIES",
                "/Library/Developer/CoreSimulator/Profiles/Runtimes/iOS 11.1.simruntime/Contents/Resources/RuntimeRoot/usr/lib/libMainThreadChecker.dylib"
            ),
            (
                "DYLD_LIBRARY_PATH",
                "__TESTROOT__/Debug-iphonesimulator:__PLATFORMS__/iPhoneSimulator.platform/Developer/Library/Frameworks:/Library/Developer/CoreSimulator/Profiles/Runtimes/iOS 11.1.simruntime/Contents/Resources/RuntimeRoot/Developer/Library/PrivateFrameworks"
            ),
            ("MTC_CRASH_ON_REPORT", "1"),
            ("XCODE_DBG_XPC_EXCLUSIONS", "com.apple.dt.xctestSymbolicator"),
        ]))
    );
    assert_eq!(target.product_module_name().expect("module"), "sample_appUITests");
    let built_products = "/Users/pkunzip/Development/marathon/vendor-ios/src/test/resources/src/sample-xcworkspace/a/Build/Products/Debug-iphonesimulator";
    assert_eq!(
        as_set(target.environment_variables().expect("env")),
        as_set(ordered(&[
            ("DYLD_FRAMEWORK_PATH", built_products),
            ("DYLD_LIBRARY_PATH", built_products),
            ("OS_ACTIVITY_DT_MODE", "YES"),
            ("__XCODE_BUILT_PRODUCTS_DIR_PATHS", built_products),
            ("__XPC_DYLD_FRAMEWORK_PATH", built_products),
            ("__XPC_DYLD_LIBRARY_PATH", built_products),
        ]))
    );
}

#[test]
fn test_write() {
    let xctestrun =
        Xctestrun::from_path(&fixture("UITesting_iphonesimulator11.2-x86_64.xctestrun")).expect("read fixture");
    let targets = xctestrun.targets();
    let target = &targets[0].1;
    target.set_testing_environment_variables(ordered(&[("X", "A"), ("Y", "B")]));
    target.set_dependent_product_paths(vec!["pathA".to_owned(), "pathB".to_owned()]);
    // The mutated target writes through the root: unknown keys, their values and their
    // positions survive (the minimally-invasive contract). Exact bytes, untrimmed.
    let expected = std::fs::read(fixture("expect_1.xctestrun")).expect("read expect_1");
    // The fixture file ends with an editor newline that the Kotlin trim() hid;
    // dd-plist writes no trailing newline (0.4b pushed bytes end at </plist>).
    let expected = expected.strip_suffix(b"\n").expect("fixture newline");
    assert_eq!(xctestrun.to_xml_bytes(), expected);
}

// The three ideas worth keeping from the commented-out legacy suite
// (xctestrun/XctestrunTest.kt), re-expressed against the v1 model per WP 4.5a
// decision 2. They have no parity rows.

#[test]
fn equality_ignores_key_order() {
    let normal = Xctestrun::from_path(&fixture("UITesting_iphonesimulator11.2-x86_64.xctestrun")).expect("read normal");
    let reordered = Xctestrun::from_path(&fixture("UITesting_iphonesimulator11.2-x86_64-reordered.xctestrun"))
        .expect("read reordered");
    assert_eq!(normal, reordered);
}

#[test]
fn metadata_key_does_not_hide_the_target() {
    let xctestrun =
        Xctestrun::from_path(&fixture("UITesting_iphonesimulator12.1-x86_64.xctestrun")).expect("read 12.1");
    let targets = xctestrun.targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].0, "sample-appUITests");
}

#[test]
fn serialize_then_parse_round_trip() {
    let first = Xctestrun::from_path(&fixture("UITesting_iphonesimulator11.2-x86_64.xctestrun")).expect("read fixture");
    let bytes = first.to_xml_bytes();
    let second = Xctestrun::from_bytes(&bytes).expect("re-read written bytes");
    assert_eq!(first, second);
    assert_eq!(second.to_xml_bytes(), bytes);
}
