//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt,
//! test/MetaProperty.kt, device/DeviceInfo.kt, execution/TestResult.kt,
//! execution/Attachment.kt and json/FileSerializer.kt.

use crate::{
    artifact_io::clock_error,
    error::VendorResult,
    model::*,
    time::{ObjectIdentity, ReplayHooks},
};
use WriterValue as V;
use jcompat::{
    jnum,
    writers::{JavaNumber, KotlinValue, WriterValue},
};

pub fn object(fields: impl IntoIterator<Item = (&'static str, V)>) -> V {
    V::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn number(text: String) -> V {
    // Only Java numeric formatters reach this boundary.
    JavaNumber::new(text.clone()).map_or_else(|_| V::String(text), V::Number)
}
pub fn status(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Failure => "FAILURE",
        TestStatus::Passed => "PASSED",
        TestStatus::Ignored => "IGNORED",
        TestStatus::Incomplete => "INCOMPLETE",
        TestStatus::AssumptionFailure => "ASSUMPTION_FAILURE",
    }
}
fn test_with(test: &Test, jackson: bool) -> V {
    object([
        ("pkg", V::String(test.pkg.clone())),
        ("clazz", V::String(test.clazz.clone())),
        ("method", V::String(test.method.clone())),
        (
            "metaProperties",
            V::Array(
                test.meta_properties
                    .iter()
                    .map(|p| {
                        object([
                            ("name", V::String(p.name.clone())),
                            ("values", meta_map(&p.values, jackson)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
fn meta_map(map: &MetaMap, jackson: bool) -> V {
    V::Object(
        map.entries()
            .iter()
            .filter(|(_, v)| !matches!(v, MetaValue::Null))
            .map(|(k, v)| (k.clone(), meta_with(v, jackson)))
            .collect(),
    )
}
pub fn meta(value: &MetaValue) -> V {
    meta_with(value, false)
}
fn meta_with(value: &MetaValue, jackson: bool) -> V {
    use MetaValue::*;
    match value {
        Null => V::Null,
        Boolean(v) => V::Bool(*v),
        Byte(v) => V::from(i64::from(*v)),
        Short(v) => V::from(i64::from(*v)),
        Int(v) => V::from(i64::from(*v)),
        Long(v) => V::from(*v),
        Char(v) => V::String(std::string::String::from_utf16_lossy(&[*v])),
        Float(v) => number(jnum::float_to_string(*v)),
        Double(v) => number(jnum::double_to_string(*v)),
        String(v) => V::String(v.clone()),
        List(v) => V::Array(v.iter().map(|v| meta_with(v, jackson)).collect()),
        Map(v) => meta_map(v, jackson),
        Array(v) => {
            if jackson && v.jvm_class == "[B" {
                let bytes: Option<Vec<_>> = v
                    .elements
                    .iter()
                    .map(|v| match v {
                        Byte(b) => Some(*b as u8),
                        _ => None,
                    })
                    .collect();
                if let Some(bytes) = bytes {
                    return V::Binary(bytes);
                }
            } else if jackson && v.jvm_class == "[C" {
                let chars: Option<Vec<_>> = v
                    .elements
                    .iter()
                    .map(|v| match v {
                        Char(c) => Some(*c),
                        _ => None,
                    })
                    .collect();
                if let Some(chars) = chars {
                    return V::String(std::string::String::from_utf16_lossy(&chars));
                }
            }
            V::Array(v.elements.iter().map(|v| meta_with(v, jackson)).collect())
        }
        Decoded(v) => decoded(v),
    }
}
fn decoded(value: &DecodedWrapper) -> V {
    use DecodedWrapper::*;
    let value = match value {
        DecodedByte(v) => V::from(i64::from(*v)),
        DecodedShort(v) => V::from(i64::from(*v)),
        DecodedInt(v) => V::from(i64::from(*v)),
        DecodedLong(v) => V::from(*v),
        DecodedFloat(v) => number(jnum::float_to_string(*v)),
        DecodedDouble(v) => number(jnum::double_to_string(*v)),
        DecodedChar(v) => V::String(std::string::String::from_utf16_lossy(&[*v])),
        DecodedBoolean(v) => V::Bool(*v),
        DecodedString(v) | DecodedType(v) | DecodedEnum(v) => V::String(v.clone()),
        DecodedNull => return object([]),
        DecodedArrayValue(v) => return object([("values", V::Array(v.iter().map(decoded).collect()))]),
    };
    object([("value", value)])
}
pub fn parse_results(tests: &[Test], flaky_tests: &[Test]) -> String {
    let mut fields = vec![("tests", V::Array(tests.iter().map(|t| test_with(t, true)).collect()))];
    if !flaky_tests.is_empty() {
        fields.push((
            "flakyTests",
            V::Array(flaky_tests.iter().map(|t| test_with(t, true)).collect()),
        ));
    }
    jcompat::writers::JacksonYamlWriter.write(&object(fields))
}

pub fn kotlin_meta(value: &MetaValue, hooks: &dyn ReplayHooks) -> VendorResult<KotlinValue> {
    use MetaValue::*;
    Ok(match value {
        Null => KotlinValue::Null,
        List(v) => KotlinValue::List(v.iter().map(|v| kotlin_meta(v, hooks)).collect::<VendorResult<_>>()?),
        Map(v) => KotlinValue::Map(
            v.entries()
                .iter()
                .map(|(k, v)| Ok((KotlinValue::Text(k.clone()), kotlin_meta(v, hooks)?)))
                .collect::<VendorResult<_>>()?,
        ),
        Array(v) => {
            let suffix = hooks
                .identity_suffix(
                    "core.RawJsonReporter.identities",
                    ObjectIdentity(std::sync::Arc::as_ptr(v) as usize as u64),
                )
                .map_err(clock_error)?;
            let mut text = v.jvm_class.clone();
            text.push('@');
            text.push_str(&suffix);
            KotlinValue::Text(text)
        }
        Decoded(v) => {
            // DecodedNull has Object.toString even inside generated data classes.
            fn has_null(v: &DecodedWrapper) -> bool {
                match v {
                    DecodedWrapper::DecodedNull => true,
                    DecodedWrapper::DecodedArrayValue(v) => v.iter().any(has_null),
                    _ => false,
                }
            }
            let suffix = if has_null(v) {
                hooks
                    .identity_suffix("core.RawJsonReporter.identities", ObjectIdentity(u64::MAX))
                    .map_err(clock_error)?
            } else {
                std::string::String::default()
            };
            v.kotlin_value(&suffix)
        }
        _ => KotlinValue::Text(match meta(value) {
            V::String(v) => v,
            V::Bool(v) => if v { "true" } else { "false" }.into(),
            V::Number(v) => v.as_str().into(),
            _ => unreachable!("scalar projection"),
        }),
    })
}
