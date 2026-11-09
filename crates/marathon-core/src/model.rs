//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt
//! and vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/DexTestParser.kt
//! plus test/, device/ and execution/ value types (see model-contract.md),
//! including execution/queue/TestAction.kt.

use crate::vendor::DeviceRef;
use jcompat::{jnum, jstring, writers::KotlinValue};
use std::path::PathBuf;
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

/// Metadata is carried to serializers but deliberately excluded from Eq/Hash.
#[derive(Clone, Debug)]
pub struct Test {
    pub pkg: String,
    pub clazz: String,
    pub method: String,
    pub meta_properties: Vec<MetaProperty>,
}
impl PartialEq for Test {
    fn eq(&self, other: &Self) -> bool {
        self.pkg == other.pkg && self.clazz == other.clazz && self.method == other.method
    }
}
impl Eq for Test {}
impl Hash for Test {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pkg.hash(state);
        self.clazz.hash(state);
        self.method.hash(state);
    }
}
/// Test.kt name extensions. Kotlin's `pkg.isNotBlank()` decides the package
/// prefix, so a whitespace-only package is dropped, not printed.
impl Test {
    /// Renders the Kotlin data-class toString.
    pub fn kotlin_value(&self, hooks: &dyn crate::time::ReplayHooks) -> crate::error::VendorResult<KotlinValue> {
        let properties = self
            .meta_properties
            .iter()
            .map(|p| {
                Ok(KotlinValue::DataClass {
                    name: "MetaProperty".into(),
                    fields: vec![
                        ("name".into(), KotlinValue::Text(p.name.clone())),
                        (
                            "values".into(),
                            crate::report::projection::kotlin_meta(&MetaValue::Map(p.values.clone()), hooks)?,
                        ),
                    ],
                })
            })
            .collect::<crate::error::VendorResult<Vec<_>>>()?;
        Ok(KotlinValue::DataClass {
            name: "Test".into(),
            fields: vec![
                ("pkg".into(), KotlinValue::Text(self.pkg.clone())),
                ("clazz".into(), KotlinValue::Text(self.clazz.clone())),
                ("method".into(), KotlinValue::Text(self.method.clone())),
                ("metaProperties".into(), KotlinValue::List(properties)),
            ],
        })
    }
    pub fn to_class_name(&self, separator: char) -> String {
        let mut name = String::default();
        if !self.pkg.chars().all(jstring::is_whitespace) {
            name.push_str(&self.pkg);
            name.push(separator);
        }
        name.push_str(&self.clazz);
        name
    }
    pub fn to_test_name(&self, package_separator: char, method_separator: char) -> String {
        let mut name = self.to_class_name(package_separator);
        name.push(method_separator);
        name.push_str(&self.method);
        name
    }
    /// `toSafeTestName()`: `toTestName(methodSeparator = '.')`.
    pub fn to_safe_test_name(&self) -> String {
        self.to_test_name('.', '.')
    }
    pub fn to_simple_safe_test_name(&self, method_separator: char) -> String {
        let mut name = self.clazz.clone();
        name.push(method_separator);
        name.push_str(&self.method);
        name
    }
}
/// Map iteration order is retained separately from Kotlin map equality.
#[derive(Clone, Debug)]
pub struct MetaProperty {
    pub name: String,
    pub values: MetaMap,
}
/// Kotlin Map: last insertion replaces a value without moving the key.
/// Private entries enforce unique keys for every constructible metadata map.
#[derive(Clone, Debug, Default)]
pub struct MetaMap {
    entries: Vec<(String, MetaValue)>,
}
impl MetaMap {
    pub fn insert(&mut self, key: String, value: MetaValue) -> Option<MetaValue> {
        if let Some((_, current)) = self.entries.iter_mut().find(|(existing, _)| *existing == key) {
            return Some(std::mem::replace(current, value));
        }
        self.entries.push((key, value));
        None
    }
    pub fn entries(&self) -> &[(String, MetaValue)] {
        &self.entries
    }
}
impl FromIterator<(String, MetaValue)> for MetaMap {
    fn from_iter<T: IntoIterator<Item = (String, MetaValue)>>(iter: T) -> Self {
        let mut map = Self::default();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}
impl PartialEq for MetaMap {
    fn eq(&self, other: &Self) -> bool {
        map_equals(&self.entries, &other.entries)
    }
}
impl Eq for MetaMap {}
/// Do not stringify annotation values at discovery. DEX arrays use identity
/// equality and JVM array toString in raw reports (C19), but JSON arrays in Gson.
#[derive(Clone, Debug)]
pub enum MetaValue {
    Null,
    Boolean(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Char(u16),
    Float(f32),
    Double(f64),
    String(String),
    List(Vec<MetaValue>),
    Map(MetaMap),
    Array(Arc<MetaArray>),
    Decoded(DecodedWrapper),
}
/// dex-test-parser 2.3.4 DecodedValue subclasses retained inside DEX arrays.
/// Array wrappers compare their backing array by identity; Null is a singleton.
#[derive(Clone, Debug)]
pub enum DecodedWrapper {
    DecodedByte(i8),
    DecodedShort(i16),
    DecodedInt(i32),
    DecodedLong(i64),
    DecodedFloat(f32),
    DecodedDouble(f64),
    DecodedChar(u16),
    DecodedBoolean(bool),
    DecodedString(String),
    DecodedType(String),
    DecodedEnum(String),
    DecodedNull,
    DecodedArrayValue(Arc<[DecodedWrapper]>),
}
impl DecodedWrapper {
    /// Project the pinned data-class toString, including Arrays.toString for
    /// nested arrays. Supply the singleton's replay identity suffix, not Debug.
    pub fn kotlin_value(&self, null_identity_suffix: &str) -> KotlinValue {
        use DecodedWrapper::*;
        let (name, value) = match self {
            DecodedByte(v) => ("DecodedByte", v.to_string()),
            DecodedShort(v) => ("DecodedShort", v.to_string()),
            DecodedInt(v) => ("DecodedInt", v.to_string()),
            DecodedLong(v) => ("DecodedLong", v.to_string()),
            DecodedFloat(v) => ("DecodedFloat", jnum::float_to_string(*v)),
            DecodedDouble(v) => ("DecodedDouble", jnum::double_to_string(*v)),
            DecodedChar(v) => ("DecodedChar", String::from_utf16_lossy(&[*v])),
            DecodedBoolean(v) => ("DecodedBoolean", if *v { "true" } else { "false" }.into()),
            DecodedString(v) => ("DecodedString", v.clone()),
            DecodedType(v) => ("DecodedType", v.clone()),
            DecodedEnum(v) => ("DecodedEnum", v.clone()),
            DecodedNull => {
                let mut text = String::from("com.linkedin.dex.parser.DecodedValue$DecodedNull@");
                text.push_str(null_identity_suffix);
                return KotlinValue::Text(text);
            }
            DecodedArrayValue(values) => {
                return KotlinValue::DataClass {
                    name: "DecodedArrayValue".into(),
                    fields: vec![(
                        "values".into(),
                        KotlinValue::List(values.iter().map(|v| v.kotlin_value(null_identity_suffix)).collect()),
                    )],
                };
            }
        };
        KotlinValue::DataClass {
            name: name.into(),
            fields: vec![("value".into(), KotlinValue::Text(value))],
        }
    }
}
impl PartialEq for DecodedWrapper {
    fn eq(&self, other: &Self) -> bool {
        use DecodedWrapper::*;
        match (self, other) {
            (DecodedByte(a), DecodedByte(b)) => a == b,
            (DecodedShort(a), DecodedShort(b)) => a == b,
            (DecodedInt(a), DecodedInt(b)) => a == b,
            (DecodedLong(a), DecodedLong(b)) => a == b,
            (DecodedFloat(a), DecodedFloat(b)) => a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
            (DecodedDouble(a), DecodedDouble(b)) => a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
            (DecodedChar(a), DecodedChar(b)) => a == b,
            (DecodedBoolean(a), DecodedBoolean(b)) => a == b,
            (DecodedString(a), DecodedString(b))
            | (DecodedType(a), DecodedType(b))
            | (DecodedEnum(a), DecodedEnum(b)) => a == b,
            (DecodedNull, DecodedNull) => true,
            (DecodedArrayValue(a), DecodedArrayValue(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}
impl Eq for DecodedWrapper {}
#[derive(Debug)]
pub struct MetaArray {
    pub jvm_class: String,
    /// DEX producers use MetaValue::Decoded for every element, without unboxing.
    pub elements: Vec<MetaValue>,
}
/// TestAction.kt: the only side effect is Complete, a test that reached a
/// terminal state under the execution strategy. Kotlin's nullable result is Option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TestAction {
    Complete,
}
/// Structural equals on tests/id; use Arc::ptr_eq on BatchRef for Kotlin ===.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestBatch {
    pub tests: Vec<Test>,
    pub id: String,
}
pub type BatchRef = Arc<TestBatch>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestShard {
    pub tests: Vec<Test>,
    pub flaky_tests: Vec<Test>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DevicePoolId {
    pub name: String,
}
#[derive(Clone, PartialEq, Eq)]
pub struct DevicePool {
    pub name: String,
    pub devices: Vec<DeviceRef>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OperatingSystem {
    pub version: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NetworkState {
    Disconnected,
    Connected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceFeature {
    Video,
    Screenshot,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidDeviceFeature(pub String);
impl DeviceFeature {
    /// Kotlin nullable JsonCreator. English case conversion; valid names are ASCII.
    pub fn from_string(key: Option<&str>) -> Result<Option<Self>, InvalidDeviceFeature> {
        match key {
            None => Ok(None),
            Some(s) if s.eq_ignore_ascii_case("VIDEO") => Ok(Some(Self::Video)),
            Some(s) if s.eq_ignore_ascii_case("SCREENSHOT") => Ok(Some(Self::Screenshot)),
            Some(s) => Err(InvalidDeviceFeature(s.into())),
        }
    }
}
/// Value snapshot; all seven fields participate in equality. There is no ABI or
/// worker ID here. D3 omits safeSerialNumber$delegate from every JSON embedding.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeviceInfo {
    pub operating_system: OperatingSystem,
    pub serial_number: String,
    pub model: String,
    pub manufacturer: String,
    pub network_state: NetworkState,
    pub device_features: Vec<DeviceFeature>,
    pub healthy: bool,
}
/// Order matches Kotlin enum ordinals; output names are uppercase with underscores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TestStatus {
    Failure,
    Passed,
    Ignored,
    Incomplete,
    AssumptionFailure,
}
/// Gson includes the eight constructor fields then isTimeInfoAvailable, computed
/// from both times != 0. Derived getters isIgnored/isSuccess/duration are absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestResult {
    pub test: Test,
    pub device: DeviceInfo,
    pub test_batch_id: String,
    pub status: TestStatus,
    pub start_time: i64,
    pub end_time: i64,
    pub stacktrace: Option<String>,
    pub attachments: Vec<Attachment>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestBatchResults {
    pub device: DeviceRef,
    pub finished: Vec<TestResult>,
    pub failed: Vec<TestResult>,
    pub uncompleted: Vec<TestResult>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub file: PathBuf,
    pub kind: AttachmentType,
    pub name: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttachmentType {
    ScreenshotGif,
    ScreenshotJpeg,
    ScreenshotPng,
    ScreenshotWebp,
    Video,
    Log,
    Profiling,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionCommand {
    Parse {
        output_file_name: Option<String>,
        include_flaky_tests: bool,
    },
    Run,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecuteMode {
    Parse,
    Run,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppType {
    Cli,
    GradlePlugin,
}

impl AppType {
    pub fn value(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::GradlePlugin => "gradle-plugin",
        }
    }
}

impl PartialEq for MetaProperty {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.values == other.values
    }
}
impl Eq for MetaProperty {}
fn map_equals(left: &[(String, MetaValue)], right: &[(String, MetaValue)]) -> bool {
    left.len() == right.len()
        && left.iter().all(|(key, value)| {
            right
                .iter()
                .find(|(other_key, _)| key == other_key)
                .is_some_and(|(_, other)| value == other)
        })
}
impl PartialEq for MetaValue {
    fn eq(&self, other: &Self) -> bool {
        use MetaValue::*;
        match (self, other) {
            (Null, Null) => true,
            (Boolean(a), Boolean(b)) => a == b,
            (Byte(a), Byte(b)) => a == b,
            (Short(a), Short(b)) => a == b,
            (Int(a), Int(b)) => a == b,
            (Long(a), Long(b)) => a == b,
            (Char(a), Char(b)) => a == b,
            (Float(a), Float(b)) => a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
            (Double(a), Double(b)) => a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
            (String(a), String(b)) => a == b,
            (List(a), List(b)) => a == b,
            (Map(a), Map(b)) => a == b,
            (Array(a), Array(b)) => Arc::ptr_eq(a, b),
            (Decoded(a), Decoded(b)) => a == b,
            _ => false,
        }
    }
}
impl Eq for MetaValue {}

impl std::ops::Add for TestStatus {
    type Output = Self;
    fn add(self, value: Self) -> Self {
        if self == Self::Passed { value } else { self }
    }
}
impl TestResult {
    pub fn is_ignored(&self) -> bool {
        matches!(self.status, TestStatus::Ignored | TestStatus::AssumptionFailure)
    }
    pub fn is_success(&self) -> bool {
        self.status == TestStatus::Passed
    }
}
