//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/DexTestParser.kt
//!
//! The `LocalTestParser` adapter over marathon-dex (WP 6.4 decision 1).
//! WP 6.5 wires the bundle factory and registration hooks to AndroidTestBundle
//! and AndroidTestBundleIdentifier. Each parsed bundle keeps reference identity.
//! `extract` below is the Kotlin flow unchanged, run on the vendor
//! runtime's blocking pool (lifecycle.md: no blocking work on the core
//! executor).

use jcompat::{file_manager::JavaPath, writers::JavaString};
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    model::{DecodedWrapper, MetaArray, MetaMap, MetaProperty, MetaValue, Test},
    runtime::{VendorFuture, VendorRuntime},
    vendor::{LocalTestParser, VendorContext},
};
use marathon_dex::{DecodedValue, DexError, TestAnnotation, TestMethod};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

/// One entry of Kotlin's `testBundlesCompat()` output, reduced to the
/// files extract() reads or carries into the registered bundle copy.
/// `split_apks` is carried and ignored by discovery, exactly as upstream
/// (only `test_application` is parsed).
#[derive(Clone, Debug)]
pub struct DexTestBundleFiles {
    pub application: Option<JavaPath>,
    pub test_application: JavaPath,
    pub extra_applications: Vec<JavaPath>,
    pub split_apks: Vec<JavaPath>,
}

/// `AndroidTestBundle(bundle.application, bundle.testApplication,
/// bundle.extraApplications, bundle.splitApks)`: called once per
/// configured bundle, after its APK parsed, and the result is shared by
/// every test of that bundle (two identically configured bundles still
/// get two distinct values).
pub type BundleFactory<B> = Arc<dyn Fn(&DexTestBundleFiles) -> B + Send + Sync>;

/// `AndroidTestBundleIdentifier.put(test, androidTestBundle)`.
pub type BundleRegistrar<B> = Arc<dyn Fn(&Test, &B) + Send + Sync>;

pub struct DexTestParser<B> {
    bundles: Vec<DexTestBundleFiles>,
    runtime: Arc<dyn VendorRuntime>,
    create_bundle: BundleFactory<B>,
    register: BundleRegistrar<B>,
}

impl<B: Send + Sync + 'static> DexTestParser<B> {
    pub fn new(
        bundles: Vec<DexTestBundleFiles>,
        runtime: Arc<dyn VendorRuntime>,
        create_bundle: BundleFactory<B>,
        register: BundleRegistrar<B>,
    ) -> Self {
        Self {
            bundles,
            runtime,
            create_bundle,
            register,
        }
    }

    /// DexTestParser.extract (DexTestParser.kt:18-45), synchronously, on
    /// the calling thread. `LocalTestParser::extract` runs it on the
    /// vendor runtime's blocking pool.
    pub fn extract_sync(&self) -> VendorResult<Vec<Test>> {
        extract_bundles(&self.bundles, &self.create_bundle, &self.register)
    }
}

fn extract_bundles<B>(
    bundles: &[DexTestBundleFiles],
    create_bundle: &BundleFactory<B>,
    register: &BundleRegistrar<B>,
) -> VendorResult<Vec<Test>> {
    let mut tests = Vec::new();
    for bundle in bundles {
        let test_application = bundle
            .test_application
            .to_file_path_buf()
            .map_err(|err| failure_from(DexError::io(err.to_string().as_str())))?;
        // bundle.testApplication.absolutePath: FileInputStream failures
        // propagate unwrapped (no try/catch in DexTestParser.kt).
        let methods = marathon_dex::find_test_methods(&test_application, &[]).map_err(failure_from)?;
        let android_test_bundle = create_bundle(bundle);
        // Arrays are shared within a bundle's results only; the converter
        // keeps every source array it has seen alive while it lives.
        let mut converter = MetaConverter::default();
        for method in &methods {
            let test = test_from_method(method, &mut converter)?;
            register(&test, &android_test_bundle);
            tests.push(test);
        }
    }
    Ok(tests)
}

impl<B: Send + Sync + 'static> LocalTestParser for DexTestParser<B> {
    fn extract(&self, _context: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        let bundles = self.bundles.clone();
        let create_bundle = Arc::clone(&self.create_bundle);
        let register = Arc::clone(&self.register);
        let runtime = Arc::clone(&self.runtime);
        Box::pin(async move {
            let output: Arc<Mutex<Option<Vec<Test>>>> = Arc::default();
            let sink = Arc::clone(&output);
            let task = runtime.spawn_blocking(Box::new(move || {
                let tests = extract_bundles(&bundles, &create_bundle, &register)?;
                *sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tests);
                Ok(())
            }));
            task.join().await?;
            let tests = output.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
            tests.ok_or_else(|| {
                VendorError::Other(Failure {
                    class: "java.lang.IllegalStateException".into(),
                    message: Some("test parsing finished without a result".into()),
                    cause: None,
                    stacktrace: None,
                })
            })
        })
    }
}

fn failure_from(err: DexError) -> VendorError {
    fn to_failure(err: DexError) -> Failure {
        Failure {
            class: err.jvm_class,
            message: err.message,
            cause: err.cause.map(|cause| Box::new(to_failure(*cause))),
            stacktrace: None,
        }
    }
    VendorError::TestParsing(to_failure(err))
}

/// The name split of extract(): "pkg.Class#method" by '#' (exactly two
/// parts), package before the LAST '.', "" when there is no dot.
pub fn test_from_method(method: &TestMethod, converter: &mut MetaConverter) -> VendorResult<Test> {
    let test_name = &method.test_name;
    let split: Vec<JavaString> = split_units(test_name, u16::from(b'#'));
    if split.len() != 2 {
        return Err(failure_from(DexError::illegal_state(concat(
            "Can't parse test ",
            test_name,
        ))));
    }
    let package_and_class = &split[0];
    let last_dot = package_and_class.0.iter().rposition(|&u| u == u16::from(b'.'));
    let (package, class) = match last_dot {
        None => (JavaString::default(), package_and_class.clone()),
        Some(index) => (
            JavaString(package_and_class.0[..index].to_vec()),
            JavaString(package_and_class.0[index + 1..].to_vec()),
        ),
    };
    let annotations = method
        .annotations
        .iter()
        .map(|annotation| converter.to_meta_property(annotation))
        .collect();
    Ok(Test {
        pkg: package,
        clazz: class,
        method: split[1].clone(),
        meta_properties: annotations,
    })
}

fn concat(prefix: &str, rest: &JavaString) -> JavaString {
    let mut out: Vec<u16> = prefix.encode_utf16().collect();
    out.extend(rest.0.iter());
    JavaString(out)
}

/// Kotlin CharSequence.split on a single char: every occurrence splits,
/// trailing empty parts are kept.
fn split_units(text: &JavaString, delimiter: u16) -> Vec<JavaString> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, &unit) in text.0.iter().enumerate() {
        if unit == delimiter {
            parts.push(JavaString(text.0[start..index].to_vec()));
            start = index + 1;
        }
    }
    parts.push(JavaString(text.0[start..].to_vec()));
    parts
}

/// TestAnnotation.toMetaProperty (DexTestParser.kt:47-67) plus the frozen
/// DecodedWrapper representation of model-contract.md (C28). The caches
/// keep the JVM's array sharing: marathon-dex shares one backing array
/// wherever the JVM shares one DecodedValue[], and the array identity is
/// what MetaValue::Array equality (and C19 raw reports) key on — so one
/// converter serves all of a bundle's tests. Entries are keyed by the
/// source array's address and hold that array, so an address cannot be
/// freed and reused for a different array while the converter lives.
#[derive(Default)]
pub struct MetaConverter {
    arrays: HashMap<usize, Converted<Arc<MetaArray>>>,
    wrappers: HashMap<usize, Converted<Arc<[DecodedWrapper]>>>,
}

/// A cache entry: the source array (held so its address stays taken) and
/// what it converted to.
type Converted<T> = (Arc<[DecodedValue]>, T);

impl MetaConverter {
    pub fn to_meta_property(&mut self, annotation: &TestAnnotation) -> MetaProperty {
        let mut values = MetaMap::default();
        for (name, value) in &annotation.values {
            values.insert(name.clone(), self.unbox_top_level(value));
        }
        MetaProperty {
            name: annotation.name.clone(),
            values,
        }
    }

    /// Only the top level is unboxed, per kind and in Kotlin's order:
    /// String/Type/Enum all become MetaValue::String (the distinction
    /// survives only inside arrays), Null becomes MetaValue::Null, and an
    /// array hands over its raw DecodedValue[] — every element a
    /// DecodedWrapper, never recursively unboxed (C28).
    fn unbox_top_level(&mut self, value: &DecodedValue) -> MetaValue {
        match value {
            DecodedValue::String(v) => MetaValue::String(v.clone()),
            DecodedValue::Byte(v) => MetaValue::Byte(*v),
            DecodedValue::Short(v) => MetaValue::Short(*v),
            DecodedValue::Char(v) => MetaValue::Char(*v),
            DecodedValue::Int(v) => MetaValue::Int(*v),
            DecodedValue::Long(v) => MetaValue::Long(*v),
            DecodedValue::Float(v) => MetaValue::Float(*v),
            DecodedValue::Double(v) => MetaValue::Double(*v),
            DecodedValue::Type(v) => MetaValue::String(v.clone()),
            DecodedValue::Null => MetaValue::Null,
            DecodedValue::Boolean(v) => MetaValue::Boolean(*v),
            DecodedValue::Enum(v) => MetaValue::String(v.clone()),
            DecodedValue::Array(values) => MetaValue::Array(self.meta_array(values)),
        }
    }

    fn meta_array(&mut self, values: &Arc<[DecodedValue]>) -> Arc<MetaArray> {
        let key = Arc::as_ptr(values) as *const DecodedValue as usize;
        if let Some((_, existing)) = self.arrays.get(&key) {
            return Arc::clone(existing);
        }
        let elements = values
            .iter()
            .map(|value| MetaValue::Decoded(self.wrap_decoded(value)))
            .collect();
        let array = Arc::new(MetaArray {
            jvm_class: "[Lcom.linkedin.dex.parser.DecodedValue;".into(),
            elements,
        });
        self.arrays.insert(key, (Arc::clone(values), Arc::clone(&array)));
        array
    }

    fn wrap_decoded(&mut self, value: &DecodedValue) -> DecodedWrapper {
        match value {
            DecodedValue::Byte(v) => DecodedWrapper::DecodedByte(*v),
            DecodedValue::Short(v) => DecodedWrapper::DecodedShort(*v),
            DecodedValue::Int(v) => DecodedWrapper::DecodedInt(*v),
            DecodedValue::Long(v) => DecodedWrapper::DecodedLong(*v),
            DecodedValue::Float(v) => DecodedWrapper::DecodedFloat(*v),
            DecodedValue::Double(v) => DecodedWrapper::DecodedDouble(*v),
            DecodedValue::Char(v) => DecodedWrapper::DecodedChar(*v),
            DecodedValue::Boolean(v) => DecodedWrapper::DecodedBoolean(*v),
            DecodedValue::String(v) => DecodedWrapper::DecodedString(v.clone()),
            DecodedValue::Type(v) => DecodedWrapper::DecodedType(v.clone()),
            DecodedValue::Enum(v) => DecodedWrapper::DecodedEnum(v.clone()),
            DecodedValue::Null => DecodedWrapper::DecodedNull,
            DecodedValue::Array(values) => {
                let key = Arc::as_ptr(values) as *const DecodedValue as usize;
                if let Some((_, existing)) = self.wrappers.get(&key) {
                    return DecodedWrapper::DecodedArrayValue(Arc::clone(existing));
                }
                let converted: Arc<[DecodedWrapper]> = values
                    .iter()
                    .map(|value| self.wrap_decoded(value))
                    .collect::<Vec<_>>()
                    .into();
                self.wrappers.insert(key, (Arc::clone(values), Arc::clone(&converted)));
                DecodedWrapper::DecodedArrayValue(converted)
            }
        }
    }
}

/// Single-annotation conversion with a fresh identity cache.
pub fn to_meta_property(annotation: &TestAnnotation) -> MetaProperty {
    MetaConverter::default().to_meta_property(annotation)
}
