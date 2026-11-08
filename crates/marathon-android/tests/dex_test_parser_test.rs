//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/AndroidTestParserTest.kt
//! (expectation only) and of DexTestParser.toMetaProperty (DexTestParser.kt:47-67).
//!
//! WP 6.4 decision 2: the Kotlin test constructs
//! VendorConfiguration.AndroidConfiguration (WP 2.3a) and
//! AndroidTestBundleIdentifier (WP 6.5), so the expectation is ported
//! here at the adapter level — same APK, same expected Test — and the
//! parity row moves to 6.5. Test equality excludes meta-properties by
//! design (frozen model contract), so they are asserted explicitly.

use jcompat::file_manager::JavaPath;
use marathon_android::dex_test_parser::{
    DexTestBundleFiles, DexTestParser, MetaConverter, test_from_method, to_meta_property,
};
use marathon_core::{
    error::VendorResult,
    executor::{CoreExecutor, Signal, hooks::RuntimeHooks},
    model::{DecodedWrapper, MetaValue, Test},
    vendor::{LocalTestParser, VendorContext},
};
use marathon_dex::{DecodedValue, TestAnnotation, TestMethod};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn js(text: &str) -> String {
    text.into()
}

fn fixture(rel: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

fn apk_path() -> String {
    fixture("vendor-android/android_test_1.apk")
}

fn bundle(test_application: &str) -> DexTestBundleFiles {
    DexTestBundleFiles {
        application: Some(JavaPath::from_text("")),
        test_application: JavaPath::from_text(test_application),
        extra_applications: Vec::new(),
        split_apks: Vec::new(),
    }
}

/// What the stand-in for AndroidTestBundle records: the bundle files it
/// was built from, and the thread the factory ran on.
struct FakeBundle {
    files: DexTestBundleFiles,
    thread: String,
}

/// One registration: the test and the bundle instance it was put against.
type Registration = (Test, Arc<FakeBundle>);

/// Runs LocalTestParser::extract on a real core executor, so extraction
/// goes through the vendor runtime's blocking pool.
fn extract(bundles: Vec<DexTestBundleFiles>) -> (VendorResult<Vec<Test>>, Vec<Registration>) {
    let registered: Arc<Mutex<Vec<Registration>>> = Arc::default();
    let sink = Arc::clone(&registered);
    let result = CoreExecutor::run(move |core| {
        Box::pin(async move {
            let parser = DexTestParser::new(
                bundles,
                core.vendor(),
                Arc::new(|files: &DexTestBundleFiles| {
                    Arc::new(FakeBundle {
                        files: files.clone(),
                        thread: std::thread::current().name().unwrap_or("").to_owned(),
                    })
                }),
                Arc::new(move |test: &Test, bundle: &Arc<FakeBundle>| {
                    sink.lock().unwrap().push((test.clone(), Arc::clone(bundle)));
                }),
            );
            let context = VendorContext {
                cancellation: Signal::new(),
                clock: core.vendor_clock(),
                hooks: RuntimeHooks::live(),
            };
            Ok(parser.extract(context).await)
        })
    })
    .unwrap();
    let registered = std::mem::take(&mut *registered.lock().unwrap());
    (result, registered)
}

#[test]
fn should_return_proper_list_of_test_methods() {
    let (extracted, registered) = extract(vec![bundle(&apk_path())]);
    let extracted = extracted.unwrap();
    let expected = Test {
        pkg: js("com.example"),
        clazz: js("MainActivityTest"),
        method: js("testText"),
        meta_properties: Vec::new(),
    };
    assert_eq!(extracted, vec![expected]);

    // The Kotlin expectation lists MetaProperty("org.junit.Test"),
    // MetaProperty("kotlin.Metadata"), MetaProperty("org.junit.runner.
    // RunWith"); equality there (and here) ignores meta-properties, so
    // the merged order — class-level first, then the method's own — is
    // asserted explicitly, as the JVM dump records it.
    let properties = &extracted[0].meta_properties;
    let names: Vec<String> = properties.iter().map(|p| p.name.to_owned()).collect();
    assert_eq!(names, ["kotlin.Metadata", "org.junit.runner.RunWith", "org.junit.Test"]);

    // org.junit.Test carries no values in this APK; RunWith's single
    // value is its runner class (DecodedType unboxed to String, C28).
    assert!(properties[2].values.entries().is_empty());
    let run_with = &properties[1].values.entries();
    assert_eq!(run_with.len(), 1);
    assert_eq!(run_with[0].0.to_owned(), "value");
    match &run_with[0].1 {
        MetaValue::String(v) => assert_eq!(v.to_owned(), "Landroid/support/test/runner/AndroidJUnit4;"),
        other => panic!("expected String, got {other:?}"),
    }

    // kotlin.Metadata's array-valued elements stay wrapped (C28): the
    // top-level array is a MetaValue::Array with the DecodedValue array
    // class, and no element is unboxed.
    let metadata = &properties[0];
    let has_wrapped_array = metadata.values.entries().iter().any(|(_, v)| match v {
        MetaValue::Array(array) => {
            array.jvm_class.to_owned() == "[Lcom.linkedin.dex.parser.DecodedValue;"
                && array.elements.iter().all(|e| matches!(e, MetaValue::Decoded(_)))
        }
        _ => false,
    });
    assert!(
        has_wrapped_array,
        "kotlin.Metadata should carry a wrapped DecodedValue array"
    );

    // Every extracted test is registered against the bundle, which was
    // built off the core thread (the blocking pool).
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].0, extracted[0]);
    assert_eq!(registered[0].1.files.test_application, JavaPath::from_text(&apk_path()));
    assert_ne!(registered[0].1.thread, "marathon-core");
}

/// AndroidTestBundle is constructed once per configured bundle and its
/// identity is what BundleRef equality keys on: two identically
/// configured bundles register their tests against two instances, and
/// all tests of one bundle against the same one.
#[test]
fn each_bundle_registers_its_tests_against_one_instance() {
    let (extracted, registered) = extract(vec![bundle(&apk_path()), bundle(&apk_path())]);
    assert_eq!(extracted.unwrap().len(), 2);
    assert_eq!(registered.len(), 2);
    assert!(!Arc::ptr_eq(&registered[0].1, &registered[1].1));

    let test_app = fixture("dex/test-app-debug-androidTest.apk");
    let (extracted, registered) = extract(vec![bundle(&test_app)]);
    assert_eq!(extracted.unwrap().len(), registered.len());
    assert!(registered.len() > 1);
    assert!(registered.iter().all(|(_, b)| Arc::ptr_eq(b, &registered[0].1)));
}

/// A failing bundle fails extract() through the blocking task, with the
/// DexParser failure (FileNotFoundException, unwrapped).
#[test]
fn missing_test_application_fails_extract() {
    let (extracted, registered) = extract(vec![bundle(&fixture("dex/no-such-file.apk"))]);
    let Err(marathon_core::error::VendorError::TestParsing(failure)) = extracted else {
        panic!("expected TestParsing failure");
    };
    assert_eq!(failure.class.to_owned(), "java.io.FileNotFoundException");
    assert!(registered.is_empty());
}

/// R1 regression: a second bundle's arrays must convert exactly as when
/// that bundle is extracted alone. A converter that outlived the first
/// bundle's arrays could hand its MetaArrays to the second bundle's
/// arrays at reused addresses.
#[test]
fn two_bundles_convert_like_separate_extractions() {
    let test_app = fixture("dex/test-app-debug-androidTest.apk");
    let second_dex = fixture("dex/abstract-class-in-second-dex.apk");
    let render = |tests: &[Test]| -> Vec<String> {
        tests
            .iter()
            .map(|t| format!("{:?} {:?}", t, t.meta_properties))
            .collect()
    };
    let (both, _) = extract(vec![bundle(&test_app), bundle(&second_dex)]);
    let (first, _) = extract(vec![bundle(&test_app)]);
    let (second, _) = extract(vec![bundle(&second_dex)]);
    let (both, first, second) = (both.unwrap(), first.unwrap(), second.unwrap());
    let mut separately = render(&first);
    separately.extend(render(&second));
    assert_eq!(render(&both), separately);

    // And no MetaArray is shared across the two bundles.
    let arrays = |tests: &[Test]| -> Vec<usize> {
        tests
            .iter()
            .flat_map(|t| t.meta_properties.iter())
            .flat_map(|p| p.values.entries().iter())
            .filter_map(|(_, v)| match v {
                MetaValue::Array(array) => Some(Arc::as_ptr(array) as usize),
                _ => None,
            })
            .collect()
    };
    let (head, tail) = both.split_at(first.len());
    let tail_arrays = arrays(tail);
    assert!(arrays(head).iter().all(|a| !tail_arrays.contains(a)));
}

fn annotation(values: Vec<(&str, DecodedValue)>) -> TestAnnotation {
    TestAnnotation {
        name: js("com.example.Annotation"),
        values: values.into_iter().map(|(k, v)| (js(k), v)).collect(),
        inherited: false,
    }
}

/// toMetaProperty unboxes only the top level (C28): scalar kinds map to
/// the boxed MetaValue (String/Type/Enum all to String), Null to Null,
/// and an array hands over wrappers verbatim.
#[test]
fn to_meta_property_unboxes_only_the_top_level() {
    let property = to_meta_property(&annotation(vec![
        ("string", DecodedValue::String(js("s"))),
        ("type", DecodedValue::Type(js("Lorg/junit/Test;"))),
        ("enum", DecodedValue::Enum(js("FAIL"))),
        ("int", DecodedValue::Int(7)),
        ("nothing", DecodedValue::Null),
        (
            "array",
            DecodedValue::Array(Arc::from(vec![
                DecodedValue::Int(7),
                DecodedValue::Null,
                DecodedValue::String(js("x")),
                DecodedValue::Array(Arc::from(vec![DecodedValue::Int(1)])),
            ])),
        ),
    ]));
    let entries = property.values.entries();
    assert_eq!(entries[0].1, MetaValue::String(js("s")));
    assert_eq!(entries[1].1, MetaValue::String(js("Lorg/junit/Test;")));
    assert_eq!(entries[2].1, MetaValue::String(js("FAIL")));
    assert_eq!(entries[3].1, MetaValue::Int(7));
    assert_eq!(entries[4].1, MetaValue::Null);
    let MetaValue::Array(array) = &entries[5].1 else {
        panic!("expected array")
    };
    assert_eq!(array.jvm_class.to_owned(), "[Lcom.linkedin.dex.parser.DecodedValue;");
    assert_eq!(array.elements[0], MetaValue::Decoded(DecodedWrapper::DecodedInt(7)));
    assert_eq!(array.elements[1], MetaValue::Decoded(DecodedWrapper::DecodedNull));
    assert_eq!(
        array.elements[2],
        MetaValue::Decoded(DecodedWrapper::DecodedString(js("x")))
    );
    // Nested arrays keep their wrapper; the element inside is NOT
    // recursively unboxed to MetaValue::Int.
    let MetaValue::Decoded(DecodedWrapper::DecodedArrayValue(inner)) = &array.elements[3] else {
        panic!("expected nested array wrapper, got {:?}", array.elements[3]);
    };
    assert_eq!(inner.as_ref(), [DecodedWrapper::DecodedInt(1)].as_slice());
    // Distinct wrapper kinds around equal strings stay distinct.
    assert_ne!(
        DecodedWrapper::DecodedString(js("a")),
        DecodedWrapper::DecodedType(js("a"))
    );
    assert_ne!(
        DecodedWrapper::DecodedString(js("a")),
        DecodedWrapper::DecodedEnum(js("a"))
    );
}

/// Array identity: one backing array shared by two annotations converts
/// to ONE MetaArray allocation (the JVM hands over the same
/// DecodedValue[]), and a fresh equal-content array converts to a
/// different, unequal one.
#[test]
fn to_meta_property_preserves_array_identity() {
    let shared: Arc<[DecodedValue]> = Arc::from(vec![DecodedValue::Int(1)]);
    let fresh: Arc<[DecodedValue]> = Arc::from(vec![DecodedValue::Int(1)]);
    let mut converter = MetaConverter::default();
    let first = converter.to_meta_property(&annotation(vec![("v", DecodedValue::Array(Arc::clone(&shared)))]));
    let second = converter.to_meta_property(&annotation(vec![("v", DecodedValue::Array(shared))]));
    let third = converter.to_meta_property(&annotation(vec![("v", DecodedValue::Array(fresh))]));
    assert_eq!(first.values.entries()[0].1, second.values.entries()[0].1);
    assert_ne!(first.values.entries()[0].1, third.values.entries()[0].1);
}

/// R1 regression: one converter fed fresh arrays that are dropped right
/// after conversion (outer and nested) must never answer from a stale
/// cache entry at a reused address.
#[test]
fn converter_never_reuses_freed_array_addresses() {
    let mut converter = MetaConverter::default();
    for i in 0..10_000 {
        let nested = DecodedValue::Array(Arc::from(vec![DecodedValue::Int(i)]));
        let property = converter.to_meta_property(&annotation(vec![
            ("outer", DecodedValue::Array(Arc::from(vec![DecodedValue::Int(i)]))),
            ("nested", DecodedValue::Array(Arc::from(vec![nested]))),
        ]));
        let entries = property.values.entries();
        let MetaValue::Array(outer) = &entries[0].1 else {
            panic!("outer array")
        };
        assert_eq!(outer.elements, [MetaValue::Decoded(DecodedWrapper::DecodedInt(i))]);
        let MetaValue::Array(holder) = &entries[1].1 else {
            panic!("holder array")
        };
        let MetaValue::Decoded(DecodedWrapper::DecodedArrayValue(inner)) = &holder.elements[0] else {
            panic!("nested wrapper")
        };
        assert_eq!(inner.as_ref(), [DecodedWrapper::DecodedInt(i)].as_slice());
    }
}

fn split(test_name: &str) -> marathon_core::error::VendorResult<Test> {
    test_from_method(
        &TestMethod {
            test_name: js(test_name),
            annotations: Vec::new(),
        },
        &mut MetaConverter::default(),
    )
}

#[test]
fn name_split_uses_hash_then_last_dot() {
    let test = split("com.example.MainActivityTest#testText").unwrap();
    assert_eq!(
        (test.pkg.to_owned(), test.clazz.to_owned(), test.method.to_owned()),
        ("com.example".into(), "MainActivityTest".into(), "testText".into())
    );

    // No dot: package is "", the whole left side is the class.
    let test = split("MainActivityTest#testText").unwrap();
    assert_eq!(
        (test.pkg.to_owned(), test.clazz.to_owned()),
        ("".into(), "MainActivityTest".into())
    );

    // Not exactly two '#' parts: IllegalStateException("Can't parse test …").
    for bad in ["noHashAtAll", "a#b#c"] {
        let err = split(bad).unwrap_err();
        let marathon_core::error::VendorError::TestParsing(failure) = err else {
            panic!("expected TestParsing")
        };
        assert_eq!(failure.class.to_owned(), "java.lang.IllegalStateException");
        assert_eq!(failure.message.unwrap().to_owned(), format!("Can't parse test {bad}"));
    }
}
