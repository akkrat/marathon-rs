//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt,
//! test/MetaProperty.kt, device/DeviceInfo.kt, execution/TestResult.kt,
//! execution/Attachment.kt and json/FileSerializer.kt (JSON output models).

use crate::model::*;
use jcompat::{file_manager::absolute_file_path, jnum};
use serde::{
    Serialize, Serializer,
    ser::{Error, SerializeMap},
};
use std::path::Path;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device<'a> {
    operating_system: OperatingSystem<'a>,
    serial_number: &'a str,
    model: &'a str,
    manufacturer: &'a str,
    network_state: &'static str,
    device_features: Vec<&'static str>,
    healthy: bool,
}
#[derive(Serialize)]
struct OperatingSystem<'a> {
    version: &'a str,
}
impl<'a> From<&'a DeviceInfo> for Device<'a> {
    fn from(device: &'a DeviceInfo) -> Self {
        Self {
            operating_system: OperatingSystem {
                version: &device.operating_system.version,
            },
            serial_number: &device.serial_number,
            model: &device.model,
            manufacturer: &device.manufacturer,
            network_state: match device.network_state {
                NetworkState::Connected => "CONNECTED",
                NetworkState::Disconnected => "DISCONNECTED",
            },
            device_features: device
                .device_features
                .iter()
                .map(|f| match f {
                    DeviceFeature::Video => "VIDEO",
                    DeviceFeature::Screenshot => "SCREENSHOT",
                })
                .collect(),
            healthy: device.healthy,
        }
    }
}
#[derive(Serialize)]
pub struct Pool<'a> {
    pub name: &'a str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestJson<'a> {
    pkg: &'a str,
    clazz: &'a str,
    method: &'a str,
    meta_properties: Vec<Property<'a>>,
}
#[derive(Serialize)]
struct Property<'a> {
    name: &'a str,
    values: MetadataMap<'a>,
}
impl<'a> From<&'a Test> for TestJson<'a> {
    fn from(test: &'a Test) -> Self {
        Self {
            pkg: &test.pkg,
            clazz: &test.clazz,
            method: &test.method,
            meta_properties: test
                .meta_properties
                .iter()
                .map(|p| Property {
                    name: &p.name,
                    values: MetadataMap(&p.values),
                })
                .collect(),
        }
    }
}
struct MetadataMap<'a>(&'a MetaMap);
impl Serialize for MetadataMap<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in self.0.entries() {
            if !matches!(value, MetaValue::Null) {
                map.serialize_entry(key, &Metadata(value))?;
            }
        }
        map.end()
    }
}
struct Metadata<'a>(&'a MetaValue);
impl Serialize for Metadata<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use MetaValue::*;
        match self.0 {
            Null => serializer.serialize_none(),
            Boolean(v) => v.serialize(serializer),
            Byte(v) => v.serialize(serializer),
            Short(v) => v.serialize(serializer),
            Int(v) => v.serialize(serializer),
            Long(v) => v.serialize(serializer),
            Char(v) => std::string::String::from_utf16_lossy(&[*v]).serialize(serializer),
            Float(v) => float(*v, serializer),
            Double(v) => double(*v, serializer),
            String(v) => v.serialize(serializer),
            List(v) => serializer.collect_seq(v.iter().map(Metadata)),
            Map(v) => MetadataMap(v).serialize(serializer),
            Array(v) => serializer.collect_seq(v.elements.iter().map(Metadata)),
            Decoded(v) => DecodedJson(v).serialize(serializer),
        }
    }
}
#[derive(Serialize)]
struct DecodedValue<T> {
    value: T,
}
#[derive(Serialize)]
struct DecodedValues<'a> {
    values: Vec<DecodedJson<'a>>,
}
struct DecodedJson<'a>(&'a DecodedWrapper);
impl Serialize for DecodedJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use DecodedWrapper::*;
        match self.0 {
            DecodedByte(v) => DecodedValue { value: v }.serialize(serializer),
            DecodedShort(v) => DecodedValue { value: v }.serialize(serializer),
            DecodedInt(v) => DecodedValue { value: v }.serialize(serializer),
            DecodedLong(v) => DecodedValue { value: v }.serialize(serializer),
            DecodedFloat(v) => DecodedValue {
                value: Metadata(&MetaValue::Float(*v)),
            }
            .serialize(serializer),
            DecodedDouble(v) => DecodedValue {
                value: Metadata(&MetaValue::Double(*v)),
            }
            .serialize(serializer),
            DecodedChar(v) => DecodedValue {
                value: String::from_utf16_lossy(&[*v]),
            }
            .serialize(serializer),
            DecodedBoolean(v) => DecodedValue { value: v }.serialize(serializer),
            DecodedString(v) | DecodedType(v) | DecodedEnum(v) => DecodedValue { value: v }.serialize(serializer),
            DecodedNull => serializer.serialize_map(Some(0))?.end(),
            DecodedArrayValue(v) => DecodedValues {
                values: v.iter().map(DecodedJson).collect(),
            }
            .serialize(serializer),
        }
    }
}
fn number<S: Serializer>(text: String, finite: bool, serializer: S) -> Result<S::Ok, S::Error> {
    if !finite {
        return Err(S::Error::custom(format!(
            "{text} is not a valid double value as per JSON specification. To override this behavior, use GsonBuilder.serializeSpecialFloatingPointValues() method."
        )));
    }
    // Native shortest float spelling can change decimal data (e.g. the smallest
    // f32 is 1e-45 instead of 1.4e-45). Retain these numeric values through serde.
    serde_json::value::RawValue::from_string(text)
        .map_err(S::Error::custom)?
        .serialize(serializer)
}
fn float<S: Serializer>(value: f32, serializer: S) -> Result<S::Ok, S::Error> {
    number(jnum::float_to_string(value), value.is_finite(), serializer)
}
fn double<S: Serializer>(value: f64, serializer: S) -> Result<S::Ok, S::Error> {
    number(jnum::double_to_string(value), value.is_finite(), serializer)
}
#[derive(Serialize)]
struct AttachmentJson<'a> {
    file: String,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}
impl<'a> AttachmentJson<'a> {
    fn new(attachment: &'a Attachment, cwd: &Path) -> Self {
        Self {
            file: absolute_file_path(&attachment.file.to_string_lossy(), &cwd.to_string_lossy())
                .to_string_lossy()
                .into_owned(),
            kind: match attachment.kind {
                AttachmentType::ScreenshotGif => "SCREENSHOT_GIF",
                AttachmentType::ScreenshotJpeg => "SCREENSHOT_JPEG",
                AttachmentType::ScreenshotPng => "SCREENSHOT_PNG",
                AttachmentType::ScreenshotWebp => "SCREENSHOT_WEBP",
                AttachmentType::Video => "VIDEO",
                AttachmentType::Log => "LOG",
                AttachmentType::Profiling => "PROFILING",
            },
            name: attachment.name.as_deref(),
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResultJson<'a> {
    test: TestJson<'a>,
    device: Device<'a>,
    test_batch_id: &'a str,
    status: &'static str,
    start_time: i64,
    end_time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    stacktrace: Option<&'a str>,
    attachments: Vec<AttachmentJson<'a>>,
    is_time_info_available: bool,
}
impl<'a> TestResultJson<'a> {
    pub fn new(result: &'a TestResult, cwd: &Path) -> Self {
        Self {
            test: (&result.test).into(),
            device: (&result.device).into(),
            test_batch_id: &result.test_batch_id,
            status: super::projection::status(result.status),
            start_time: result.start_time,
            end_time: result.end_time,
            stacktrace: result.stacktrace.as_deref(),
            attachments: result.attachments.iter().map(|a| AttachmentJson::new(a, cwd)).collect(),
            is_time_info_available: result.start_time != 0 && result.end_time != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_preserves_null_omission_array_nulls_types_and_order() {
        let map: MetaMap = [
            ("missing".into(), MetaValue::Null),
            ("integer".into(), MetaValue::Long(i64::MAX)),
            ("floating".into(), MetaValue::Double(1.0)),
            ("tiny".into(), MetaValue::Float(f32::from_bits(1))),
            ("negativeZero".into(), MetaValue::Float(-0.0)),
            (
                "array".into(),
                MetaValue::List(vec![MetaValue::Null, MetaValue::Decoded(DecodedWrapper::DecodedNull)]),
            ),
            (
                "nested".into(),
                MetaValue::Map([("absent".into(), MetaValue::Null)].into_iter().collect()),
            ),
        ]
        .into_iter()
        .collect();
        let text = serde_json::to_string(&MetadataMap(&map)).unwrap();
        assert_eq!(
            text,
            r#"{"integer":9223372036854775807,"floating":1.0,"tiny":1.4E-45,"negativeZero":-0.0,"array":[null,{}],"nested":{}}"#
        );
    }

    #[test]
    fn metadata_rejects_all_nonfinite_float_and_decoded_values() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let floats = [
                MetaValue::Double(value),
                MetaValue::Float(value as f32),
                MetaValue::Decoded(DecodedWrapper::DecodedDouble(value)),
                MetaValue::Decoded(DecodedWrapper::DecodedFloat(value as f32)),
            ];
            for value in floats {
                assert!(serde_json::to_string(&Metadata(&value)).is_err());
                assert!(serde_json::to_string(&Metadata(&MetaValue::List(vec![value]))).is_err());
            }
        }
    }
}
