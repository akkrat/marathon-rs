//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt and
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/Configuration.kt
//! (Kotlin generated data-class strings and collection rendering).

/// Text is an already-rendered scalar: strings are unquoted; File/Regex/time,
/// enums, numeric formatters and explicit secret-masking overrides supply text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KotlinValue {
    Null,
    Text(String),
    DataClass {
        name: String,
        fields: Vec<(String, KotlinValue)>,
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
        Self::Text(format!("{class}@{:x}", hash as u32))
    }

    pub fn render(&self) -> String {
        let mut out = String::default();
        self.append(&mut out);
        out
    }
    fn append(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Text(s) => out.push_str(s),
            Self::DataClass { name, fields } => {
                out.push_str(name);
                out.push('(');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(key);
                    out.push('=');
                    value.append(out);
                }
                out.push(')');
            }
            Self::Map(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    k.append(out);
                    out.push('=');
                    v.append(out);
                }
                out.push('}');
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
