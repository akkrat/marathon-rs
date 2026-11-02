//! Port of core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt
//! (Gson 2.11 JsonWriter and default type-adapter output).
use super::{JavaString, WriterValue};

#[derive(Clone, Copy, Debug, Default)]
pub struct GsonWriter {
    /// GsonBuilder.setPrettyPrinting(); production marathon uses compact output.
    pub pretty: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GsonError(pub String);
impl std::fmt::Display for GsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for GsonError {}
impl GsonWriter {
    pub fn write(&self, value: &WriterValue) -> Result<JavaString, GsonError> {
        let mut out = JavaString::default();
        self.value(value, &mut out, 0)?;
        Ok(out)
    }
    fn newline(&self, out: &mut JavaString, depth: usize) {
        if self.pretty {
            out.push_str("\n");
            out.push_str(&"  ".repeat(depth));
        }
    }
    fn value(&self, value: &WriterValue, out: &mut JavaString, depth: usize) -> Result<(), GsonError> {
        match value {
            WriterValue::Null => out.push_str("null"),
            WriterValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            WriterValue::Number(n) => {
                if matches!(n.as_str(), "NaN" | "-NaN" | "Infinity" | "-Infinity") {
                    return Err(GsonError(format!(
                        "{} is not a valid double value as per JSON specification. To override this behavior, use GsonBuilder.serializeSpecialFloatingPointValues() method.",
                        n.as_str()
                    )));
                }
                out.push_str(n.as_str());
            }
            WriterValue::String(s) => string(s, out),
            WriterValue::Binary(bytes) => self.value(
                &WriterValue::Array(bytes.iter().map(|b| WriterValue::from(i64::from(*b as i8))).collect()),
                out,
                depth,
            )?,
            WriterValue::Array(values) => {
                out.push_str("[");
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",");
                    }
                    self.newline(out, depth + 1);
                    self.value(v, out, depth + 1)?;
                }
                if !values.is_empty() {
                    self.newline(out, depth);
                }
                out.push_str("]");
            }
            WriterValue::Object(entries) => {
                out.push_str("{");
                let mut first = true;
                for (key, value) in entries.iter().filter(|(_, v)| !matches!(v, WriterValue::Null)) {
                    if !first {
                        out.push_str(",");
                    }
                    first = false;
                    self.newline(out, depth + 1);
                    string(key, out);
                    out.push_str(if self.pretty { ": " } else { ":" });
                    self.value(value, out, depth + 1)?;
                }
                if !first {
                    self.newline(out, depth);
                }
                out.push_str("}");
            }
        }
        Ok(())
    }
}
fn string(text: &JavaString, out: &mut JavaString) {
    out.push_str("\"");
    for &c in &text.0 {
        match c {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            8 => out.push_str("\\b"),
            9 => out.push_str("\\t"),
            10 => out.push_str("\\n"),
            12 => out.push_str("\\f"),
            13 => out.push_str("\\r"),
            0..=31 | 0x3c | 0x3e | 0x26 | 0x3d | 0x27 | 0x2028 | 0x2029 => out.push_str(&format!("\\u{c:04x}")),
            _ => out.0.push(c),
        }
    }
    out.push_str("\"");
}
