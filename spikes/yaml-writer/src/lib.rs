//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommand.kt
//! and configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt.
//!
//! Design-stop adapter only: retain today's projections to isolate emitter behaviour.
use jcompat::writers::WriterValue;
use serde::{
    Serialize, Serializer,
    ser::{Error, SerializeMap, SerializeSeq},
};

pub struct Projection<'a>(pub &'a WriterValue);

impl Serialize for Projection<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            WriterValue::Null => serializer.serialize_none(),
            WriterValue::Bool(v) => serializer.serialize_bool(*v),
            WriterValue::String(v) => serializer.serialize_str(v),
            WriterValue::Number(v) => {
                let text = v.as_str();
                // Jackson's undotted special values resolve as strings, including in parse output.
                if matches!(text, "NaN" | "-NaN" | "Infinity" | "-Infinity") {
                    serializer.serialize_str(text)
                } else if text.contains(['.', 'e', 'E']) {
                    serializer.serialize_f64(text.parse().map_err(S::Error::custom)?)
                } else {
                    serializer.serialize_i64(text.parse().map_err(S::Error::custom)?)
                }
            }
            WriterValue::Binary(v) => serializer.serialize_bytes(v),
            WriterValue::Array(items) => {
                let mut sequence = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    sequence.serialize_element(&Projection(item))?;
                }
                sequence.end()
            }
            WriterValue::Object(entries) => {
                let mut map = serializer.serialize_map(None)?;
                for (key, value) in entries {
                    if !matches!(value, WriterValue::Null) {
                        map.serialize_entry(key, &Projection(value))?;
                    }
                }
                map.end()
            }
        }
    }
}

pub fn write(value: &WriterValue) -> Result<String, serde_saphyr::ser_error::Error> {
    serde_saphyr::to_string_with_options(
        &Projection(value),
        serde_saphyr::ser_options! { yaml_12: false, prefer_block_scalars: false },
    )
}
