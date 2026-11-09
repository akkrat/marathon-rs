//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/model/TestIdentifierTest.kt,
//! model/AndroidTestStatusTest.kt, extension/TestKtTest.kt and RecorderTypeSelectorTest.kt.
mod support;
use marathon_adb::client::Request;
use marathon_android::{models::*, requests::*, version::AndroidVersion};
use marathon_config::vendor::AndroidRecorderType;
use marathon_core::model::{DeviceFeature as F, MetaMap, MetaProperty, TestStatus};
use support::*;
#[test]
fn test_to_test() {
    let id = TestIdentifier {
        class_name: js("com.example.Class"),
        test_name: js("method"),
    };
    let t = id.to_test(vec![]);
    assert_eq!(t.pkg, js("com.example"));
    assert_eq!(t.clazz, js("Class"));
    assert_eq!(t.method, js("method"));
    assert_eq!(id.class_name, js("com.example.Class"));
    assert_eq!(id.test_name, js("method"));
}
#[test]
fn test_no_package() {
    let id = TestIdentifier {
        class_name: js("Class under test"),
        test_name: js("method"),
    };
    let t = id.to_test(vec![]);
    assert_eq!(t.pkg, js(""));
    assert_eq!(t.clazz, id.class_name);
    assert_eq!(t.method, id.test_name);
}
#[test]
fn test_all() {
    for (android, core) in [
        (AndroidTestStatus::Failure, TestStatus::Failure),
        (AndroidTestStatus::Passed, TestStatus::Passed),
        (AndroidTestStatus::Incomplete, TestStatus::Incomplete),
        (AndroidTestStatus::AssumptionFailure, TestStatus::AssumptionFailure),
        (AndroidTestStatus::Ignored, TestStatus::Ignored),
    ] {
        assert_eq!(android.to_marathon_status(), core)
    }
}
#[test]
fn test_ignored() {
    let mut t = test();
    assert!(!is_ignored(&t));
    t.meta_properties.push(MetaProperty {
        name: js("org.junit.Ignore"),
        values: MetaMap::default(),
    });
    assert!(is_ignored(&t));
}
fn recorder(features: &[F], preferred: Option<AndroidRecorderType>) -> Option<F> {
    let mut c = configuration();
    let a = android(&mut c);
    a.screen_record_configuration.preferable_recorder_type = preferred;
    select_recorder_type(features, &a.screen_record_configuration)
}
#[test]
fn test_default() {
    assert_eq!(recorder(&[F::Video, F::Screenshot], None), Some(F::Video))
}
#[test]
fn test_preferred_screenshot() {
    assert_eq!(
        recorder(&[F::Video, F::Screenshot], Some(AndroidRecorderType::Screenshot)),
        Some(F::Screenshot)
    )
}
#[test]
fn test_preferred_video() {
    assert_eq!(
        recorder(&[F::Video, F::Screenshot], Some(AndroidRecorderType::Video)),
        Some(F::Video)
    )
}
#[test]
fn test_preferred_video_but_unavailable() {
    assert_eq!(
        recorder(&[F::Screenshot], Some(AndroidRecorderType::Video)),
        Some(F::Screenshot)
    )
}
#[test]
fn test_none_available() {
    assert_eq!(recorder(&[], None), None)
}
#[test]
fn test_get_all() {
    assert_eq!(GetPropRequest.serialize(), b"0016shell:getprop;echo x$?")
}
#[test]
fn test_returns_proper_content() {
    assert_eq!(
        checked(parse_props(&js("[testing]: [testing]\r\r\n")))["testing"],
        js("testing")
    );
    let props = checked(parse_props(&js(
        "# comment\r\n[ro.sdk]: [27]\n[empty]: []\n[bad]: [nested[]]\n[ro.sdk]: [28]\n",
    )));
    assert_eq!(props.len(), 2);
    assert_eq!(props["ro.sdk"], js("28"));
    assert_eq!(props["empty"], js(""));
}
#[test]
fn test_mode_arguments() {
    for mode in ["brief", "long", "process", "raw", "tag", "thread", "threadtime", "time"] {
        let bytes = logcat_request(&[mode]).serialize();
        assert_eq!(&bytes[4..], format!("shell:logcat -v {mode}").as_bytes());
    }
}
#[test]
fn test_continuous() {
    assert_eq!(&logcat_request(&["long"]).serialize()[4..], b"shell:logcat -v long");
    assert_eq!(logcat_request(&["long"]).socket_idle_timeout(), Some(i64::MAX));
}
#[test]
fn jvm_version_and_address_rows() {
    let rows: Vec<serde_json::Value> = include_str!("../../../fixtures/oracle/android-provider-probes.jsonl")
        .lines()
        .map(|s| checked(serde_json::from_str(s)))
        .collect();
    for row in rows {
        let case = row["case"].as_str().unwrap();
        if let Some(rest) = case.strip_prefix("version:") {
            let version = if rest == "default" {
                AndroidVersion::default()
            } else {
                let (sdk, code) = rest.split_once(':').unwrap();
                AndroidVersion::new(checked(sdk.parse()), code)
            };
            let text = format!(
                "{}|{}|{}|{}",
                version.api_level,
                version
                    .codename
                    .as_ref()
                    .map(|s| s.to_owned())
                    .unwrap_or_else(|| "null".into()),
                version.feature_level(),
                version.api_string_without_extension().to_owned()
            );
            let text = if rest == "default" {
                text
            } else {
                format!("{text}|{}", version.is_greater_or_equal_than(19))
            };
            assert_eq!(text, row["value"].as_str().unwrap());
        } else if let Some(ip) = case.strip_prefix("address:")
            && ip != "localhost"
        {
            assert_eq!(
                marathon_android::device::host_address(checked(ip.parse())),
                row["value"].as_str().unwrap()
            )
        }
    }
}
#[test]
fn should_return_proper_list_of_test_methods() {
    let mut c = configuration();
    android(&mut c).test_application_output = Some(std::path::PathBuf::from(
        fixture("vendor-android/android_test_1.apk")
            .to_string_lossy()
            .into_owned(),
    ));
    run(c, |runtime, context| {
        Box::pin(async move {
            let services = marathon_android::services::create_services(runtime.dependencies.clone()).await?;
            let marathon_core::vendor::TestParser::Local(parser) = services.parser.unwrap() else {
                panic!("local parser")
            };
            let tests = parser.extract(context).await?;
            assert_eq!(tests.len(), 1);
            assert_eq!(
                tests[0].to_test_name('.', '#'),
                js("com.example.MainActivityTest#testText")
            );
            let identifier = services.bundle_identifier.unwrap();
            let bundle = identifier.identify(&tests[0])?.unwrap();
            assert!(bundle.0.id().to_owned().ends_with("android_test_1.apk"));
            assert!(bundle == identifier.identify(&tests[0])?.unwrap());
            let error = identifier.identify(&test()).err().unwrap();
            assert_eq!(error.failure().kind.description(), js("configuration rejected"));
            assert_eq!(
                error.failure().message,
                Some(js("Invalid test pkg.clazz#method: can't locate test bundle"))
            );
            Ok(())
        })
    });
}
#[test]
fn logcat_keeps_partial_lines_and_split_crlf() {
    let mut lines = LogcatLines::default();
    assert!(lines.push("hel").is_empty());
    assert!(lines.push("lo\r").is_empty());
    assert_eq!(lines.push("\nnext\rlast"), vec![js("hello"), js("next")]);
    assert_eq!(lines.finish(), vec![js("last")]);
    assert!(include_str!("../../../fixtures/oracle/android-provider-probes.jsonl").contains("hel|lo|"));
}

#[test]
fn reversed_getprop_brackets_use_java_string_exception() {
    for (input, message) in [
        ("][: [value]", "Range [2, 0) out of bounds for length 11"),
        ("[key]: ][", "Range [9, 7) out of bounds for length 9"),
    ] {
        let error = parse_props(&js(input)).unwrap_err();
        assert!(matches!(&error, marathon_adb::client::AdbError::StringIndexOutOfBounds(value) if value == message));
        assert_eq!(
            error.failure_kind(),
            marathon_error::FailureKind::StringIndexOutOfBounds
        );
    }
}
