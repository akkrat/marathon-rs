//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt and
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/Configuration.kt
//! (Kotlin generated data-class strings and collection rendering).
use super::JavaString;

/// Text is an already-rendered scalar: strings are unquoted; File/Regex/time,
/// enums, numeric formatters and explicit secret-masking overrides supply text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KotlinValue {
    Null,
    Text(JavaString),
    DataClass {
        name: JavaString,
        fields: Vec<(JavaString, KotlinValue)>,
    },
    List(Vec<KotlinValue>),
    Map(Vec<(KotlinValue, KotlinValue)>),
    Tuple(Vec<KotlinValue>),
}
impl From<&str> for KotlinValue {
    fn from(s: &str) -> Self {
        Self::Text(s.into())
    }
}
impl KotlinValue {
    /// D12/F29: plain JVM objects use the class-name hash in external text.
    pub fn object(class: &str) -> Self {
        let hash = class
            .encode_utf16()
            .fold(0_i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)));
        Self::Text(format!("{class}@{:x}", hash as u32).into())
    }

    pub fn render(&self) -> JavaString {
        let mut out = JavaString::default();
        self.append(&mut out);
        out
    }
    fn append(&self, out: &mut JavaString) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Text(s) => out.0.extend_from_slice(&s.0),
            Self::DataClass { name, fields } => {
                out.0.extend_from_slice(&name.0);
                out.push_str("(");
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.0.extend_from_slice(&key.0);
                    out.push_str("=");
                    value.append(out);
                }
                out.push_str(")");
            }
            Self::Map(entries) => {
                out.push_str("{");
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    k.append(out);
                    out.push_str("=");
                    v.append(out);
                }
                out.push_str("}");
            }
            Self::List(values) | Self::Tuple(values) => {
                let tuple = matches!(self, Self::Tuple(_));
                out.push_str(if tuple { "(" } else { "[" });
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.append(out);
                }
                out.push_str(if tuple { ")" } else { "]" });
            }
        }
    }
}
