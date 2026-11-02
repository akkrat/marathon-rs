//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/xctestrun/v0/XctestrunTest.kt

use std::path::PathBuf;

use marathon_apple::plist::PropertyList;
use marathon_apple::xctestrun::v0::{TestTarget, TestTargetParams, Xctestrun};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vendor-apple-base/fixtures/xctestrun")
        .join(name)
}

fn ordered(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn sample_target() -> TestTarget {
    let mut params = TestTargetParams::new(ordered(&[
        (
            "DYLD_FRAMEWORK_PATH",
            "__TESTROOT__:__PLATFORMS__/iPhoneOS.platform/Developer/Library/Frameworks",
        ),
        (
            "DYLD_LIBRARY_PATH",
            "__TESTROOT__:__PLATFORMS__/iPhoneOS.platform/Developer/Library/Frameworks",
        ),
        ("XCODE_DBG_XPC_EXCLUSIONS", "com.apple.dt.xctestSymbolicator"),
    ]));
    params.test_bundle_path = Some("__TESTHOST__/PlugIns/Tests.xctest".to_owned());
    params.test_host_path = Some("__TESTROOT__/App-Runner.app".to_owned());
    let target = TestTarget::with_params(params);
    target.set_ui_target_app_path(Some("__TESTROOT__/Target.app".to_owned()));
    target
}

#[test]
fn test_read() {
    let actual = Xctestrun::from_path(&fixture("man_v0.xctestrun")).expect("read man_v0");
    let expected = Xctestrun::new("TestTargetName", &sample_target());
    assert_eq!(actual, expected);
}

#[test]
fn test_write() {
    // The Kotlin test writes the *target* (not the wrapper) and compares trimmed text;
    // we claim byte-exactness, so the comparison is on exact bytes.
    let expected = std::fs::read(fixture("expect_0.xctestrun")).expect("read expect_0");
    // The fixture file ends with an editor newline that the Kotlin trim() hid;
    // dd-plist writes no trailing newline (0.4b pushed bytes end at </plist>).
    let expected = expected.strip_suffix(b"\n").expect("fixture newline");
    assert_eq!(sample_target().to_xml_bytes(), expected);
}
