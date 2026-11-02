//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/ApkParserTest.kt

use marathon_dex::InstrumentationInfo;
use std::path::PathBuf;

#[test]
fn should_parser_android_manifest_and_return_instrumentation_info() {
    let apk_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vendor-android/android_test_1.apk");
    assert_eq!(
        marathon_dex::parse_instrumentation_info(&apk_file).unwrap(),
        InstrumentationInfo {
            application_package: "com.example".into(),
            instrumentation_package: "com.example.test".into(),
            test_runner_class: "android.support.test.runner.AndroidJUnitRunner".into(),
        }
    );
}
