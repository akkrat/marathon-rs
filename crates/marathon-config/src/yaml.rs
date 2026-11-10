//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! and core/src/main/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommand.kt.

use serde::{Serialize, Serializer, ser::SerializeMap};
pub use serde_saphyr::ser_error::Error;

// NON_NULL omits absent mapping properties, while null sequence elements remain data.
const OMIT_NULL_MAPPING_ENTRIES: bool = true;
pub fn omit_none<T>(value: &Option<T>) -> bool {
    OMIT_NULL_MAPPING_ENTRIES && value.is_none()
}
pub fn include_entry(is_null: bool) -> bool {
    !(OMIT_NULL_MAPPING_ENTRIES && is_null)
}

// empty_as_braces is the setting most likely to break on the next serde-saphyr upgrade.
// If a replacement is documented, pin its explicit []/{} mode and keep the compatibility test.
#[allow(deprecated)] // Pin explicit empty collections even though the option is deprecated.
fn options() -> serde_saphyr::SerializerOptions {
    serde_saphyr::ser_options! {
        // Marathonfiles and downstream consumers resolve YAML 1.1 scalars, including yes/on and timestamps.
        yaml_12: false,
        // Quoted escapes preserve all line breaks and trailing newlines across YAML 1.1 readers.
        prefer_block_scalars: false,
        // A blank collection would resolve as null, losing both its type and its emptiness.
        empty_as_braces: true,
    }
}

pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    serde_saphyr::to_string_with_options(&value, options())
}

macro_rules! float {
    ($name:ident, $ty:ty, $format:path) => {
        pub struct $name(pub $ty);
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                // Today's undotted non-finite spellings are YAML strings, not YAML floats.
                if self.0.is_nan() {
                    serializer.serialize_str("NaN")
                } else if self.0 == <$ty>::INFINITY {
                    serializer.serialize_str("Infinity")
                } else if self.0 == <$ty>::NEG_INFINITY {
                    serializer.serialize_str("-Infinity")
                } else {
                    // Preserve decimal data as well as the binary float (notably 4.9E-324).
                    // The explicit core tag keeps a quoted numeric spelling a YAML float.
                    serde_saphyr::Tagged($format(self.0), Some("tag:yaml.org,2002:float".into())).serialize(serializer)
                }
            }
        }
    };
}
float!(Float, f64, jcompat::jnum::double_to_string);
float!(Float32, f32, jcompat::jnum::float_to_string);

/// Explicit binary tags also keep bytes binary when nested in a sequence.
pub struct Binary<'a>(pub &'a [u8]);
impl Serialize for Binary<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Bytes<'a>(&'a [u8]);
        impl Serialize for Bytes<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_bytes(self.0)
            }
        }
        serde_saphyr::Tagged(Bytes(self.0), Some("tag:yaml.org,2002:binary".into())).serialize(serializer)
    }
}

/// A dynamic mapping retains insertion order without turning into a sequence of pairs.
pub struct Entries<K, V>(pub Vec<(K, V)>);
impl<K: Serialize, V: Serialize> Serialize for Entries<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(deprecated)]
    fn compatibility_settings_preserve_yaml_11_strings_lines_collections_and_nulls() -> Result<(), Error> {
        let settings = options();
        assert!(!settings.yaml_12);
        assert!(!settings.prefer_block_scalars);
        assert!(settings.empty_as_braces);
        #[derive(Serialize)]
        struct Settings<'a> {
            hazard: &'a str,
            lines: &'a str,
            list: Vec<i64>,
            map: Entries<&'a str, i64>,
            #[serde(skip_serializing_if = "omit_none")]
            missing: Option<i64>,
            nulls: [Option<i64>; 1],
        }
        assert_eq!(
            to_string(&Settings {
                hazard: "yes",
                lines: "one\ntwo\n",
                list: vec![],
                map: Entries(vec![]),
                missing: None,
                nulls: [None],
            })?,
            "hazard: \"yes\"\nlines: \"one\\ntwo\\n\"\nlist: []\nmap: {}\nnulls:\n- null\n"
        );
        for hazard in [
            "yes",
            "NO",
            "On",
            "off",
            "y",
            "N",
            "yEs",
            "oFf",
            "~",
            "null",
            "0123",
            "1_000",
            "1:20",
            ".inf",
            "2001-12-15",
            "2001-12-15T02:59:43Z",
            "<<",
            "=",
        ] {
            let stream = crate::node::load(&to_string(&hazard)?);
            assert!(stream.error.is_none(), "{hazard}");
            assert!(
                matches!(&stream.documents[0].root.kind,
                crate::node::NodeKind::Scalar { style: crate::node::Style::SingleQuoted | crate::node::Style::DoubleQuoted, text } if text == hazard),
                "{hazard}"
            );
        }
        assert!(!include_entry(true));
        assert!(include_entry(false));
        Ok(())
    }
}
