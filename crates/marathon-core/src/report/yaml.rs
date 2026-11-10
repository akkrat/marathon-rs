//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommand.kt,
//! execution/command/parse/ParseCommandResult.kt, test/Test.kt and test/MetaProperty.kt.
use crate::model::*;
use marathon_config::yaml;
use serde::{Serialize, Serializer, ser::SerializeMap};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseResults<'a> {
    tests: Vec<TestYaml<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    flaky_tests: Vec<TestYaml<'a>>,
}
impl<'a> ParseResults<'a> {
    pub fn new(tests: &'a [Test], flaky_tests: &'a [Test]) -> Self {
        Self {
            tests: tests.iter().map(TestYaml::from).collect(),
            flaky_tests: flaky_tests.iter().map(TestYaml::from).collect(),
        }
    }
}
pub fn parse_results(tests: &[Test], flaky_tests: &[Test]) -> Result<String, yaml::Error> {
    yaml::to_string(&ParseResults::new(tests, flaky_tests))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestYaml<'a> {
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
impl<'a> From<&'a Test> for TestYaml<'a> {
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
            if yaml::include_entry(matches!(value, MetaValue::Null)) {
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
            Float(v) => yaml::Float32(*v).serialize(serializer),
            Double(v) => yaml::Float(*v).serialize(serializer),
            String(v) => v.serialize(serializer),
            List(v) => serializer.collect_seq(v.iter().map(Metadata)),
            Map(v) => MetadataMap(v).serialize(serializer),
            Array(v) => {
                if v.jvm_class == "[B" {
                    let bytes: Option<Vec<_>> = v
                        .elements
                        .iter()
                        .map(|e| match e {
                            Byte(b) => Some(*b as u8),
                            _ => None,
                        })
                        .collect();
                    if let Some(bytes) = bytes {
                        return yaml::Binary(&bytes).serialize(serializer);
                    }
                } else if v.jvm_class == "[C" {
                    let chars: Option<Vec<_>> = v
                        .elements
                        .iter()
                        .map(|e| match e {
                            Char(c) => Some(*c),
                            _ => None,
                        })
                        .collect();
                    if let Some(chars) = chars {
                        return std::string::String::from_utf16_lossy(&chars).serialize(serializer);
                    }
                }
                serializer.collect_seq(v.elements.iter().map(Metadata))
            }
            Decoded(v) => DecodedYaml(v).serialize(serializer),
        }
    }
}
#[derive(Serialize)]
struct DecodedValue<T> {
    value: T,
}
#[derive(Serialize)]
struct DecodedValues<'a> {
    values: Vec<DecodedYaml<'a>>,
}
struct DecodedYaml<'a>(&'a DecodedWrapper);
impl Serialize for DecodedYaml<'_> {
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
                values: v.iter().map(DecodedYaml).collect(),
            }
            .serialize(serializer),
        }
    }
}
