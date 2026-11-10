//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/AnnotationDataFilter.kt

use super::{FilterDependencies, single::matches_regex};
use crate::{
    error::OperationResult,
    model::{DecodedWrapper, MetaValue, Test},
};
use jcompat::{JavaRegex, jnum};

#[derive(Default)]
pub struct AnnotationRenderer;
impl AnnotationRenderer {
    pub fn render(&self, value: &MetaValue) -> String {
        annotation_text(value)
    }
}

/// The `annotationData.valueRegex` text contract (D58, PLAN §3.12.1).
/// Strings and map keys are verbatim, unquoted and unescaped. Null is `null`,
/// booleans are lowercase, integers are decimal, and floats retain the `jnum`
/// spellings (including signed zero, nonfinite values and exponents). Char is
/// one UTF-16 unit decoded lossily, so a surrogate becomes U+FFFD (D55).
/// Lists and arrays are `[a, b]`, maps are `{key=value, next=value}` in entry
/// order; empty collections are `[]` / `{}`. Elements render recursively.
/// Decoded values follow [`decoded_text`]. No whitespace is trimmed or escaped.
///
/// The filter skips unboxed top-level null. An outer Array eagerly renders its
/// elements and matches each through [`array_element_value`]; other kinds match
/// their complete text. Lists do not get outer-array element extraction.
/// Raw reports share this grammar, with their own unboxed-null override.
pub fn annotation_text(value: &MetaValue) -> String {
    use MetaValue::*;
    match value {
        Null => "null".into(),
        Boolean(v) => v.to_string(),
        Byte(v) => v.to_string(),
        Short(v) => v.to_string(),
        Int(v) => v.to_string(),
        Long(v) => v.to_string(),
        Char(v) => std::string::String::from_utf16_lossy(&[*v]),
        Float(v) => jnum::float_to_string(*v),
        Double(v) => jnum::double_to_string(*v),
        String(v) => v.clone(),
        List(values) => format!(
            "[{}]",
            values.iter().map(annotation_text).collect::<Vec<_>>().join(", ")
        ),
        Array(array) => format!(
            "[{}]",
            array
                .elements
                .iter()
                .map(annotation_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Map(map) => format!(
            "{{{}}}",
            map.entries()
                .iter()
                .map(|(key, value)| format!("{key}={}", annotation_text(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Decoded(wrapper) => decoded_text(wrapper),
    }
}

/// Decoded grammar within the `annotationData` filter's text contract:
/// `DecodedByte(value=…)`, `DecodedShort(value=…)`, `DecodedInt(value=…)`,
/// `DecodedLong(value=…)`, `DecodedFloat(value=…)`, `DecodedDouble(value=…)`,
/// `DecodedChar(value=…)`, `DecodedBoolean(value=…)`, `DecodedString(value=…)`,
/// `DecodedType(value=…)` and `DecodedEnum(value=…)` use the scalar rules of
/// [`annotation_text`]. `DecodedNull` is `null` and nested arrays are
/// `DecodedArrayValue(values=[…])`, recursively separated by `, `.
/// In an outer Array, extraction starts after the first `(value=` (if any)
/// and ends before the first `)` in that tail. Thus `DecodedString(value=a)b)`
/// matches `a`, and a nested decoded array can match its first scalar's text.
pub fn decoded_text(value: &DecodedWrapper) -> String {
    use DecodedWrapper::*;
    let (name, text) = match value {
        DecodedByte(v) => ("DecodedByte", v.to_string()),
        DecodedShort(v) => ("DecodedShort", v.to_string()),
        DecodedInt(v) => ("DecodedInt", v.to_string()),
        DecodedLong(v) => ("DecodedLong", v.to_string()),
        DecodedFloat(v) => ("DecodedFloat", jnum::float_to_string(*v)),
        DecodedDouble(v) => ("DecodedDouble", jnum::double_to_string(*v)),
        DecodedChar(v) => ("DecodedChar", String::from_utf16_lossy(&[*v])),
        DecodedBoolean(v) => ("DecodedBoolean", v.to_string()),
        DecodedString(v) => ("DecodedString", v.clone()),
        DecodedType(v) => ("DecodedType", v.clone()),
        DecodedEnum(v) => ("DecodedEnum", v.clone()),
        DecodedNull => return "null".into(),
        DecodedArrayValue(values) => {
            return format!(
                "DecodedArrayValue(values=[{}])",
                values.iter().map(decoded_text).collect::<Vec<_>>().join(", ")
            );
        }
    };
    format!("{name}(value={text})")
}

pub(super) fn matches(
    test: &Test,
    name: &JavaRegex,
    value: &JavaRegex,
    dependencies: &FilterDependencies,
) -> OperationResult<bool> {
    for property in &test.meta_properties {
        if !matches_regex(name, &property.name, "annotation-data.name")? {
            continue;
        }
        let Some((_, data)) = property.values.entries().iter().find(|(k, _)| *k == "value") else {
            continue;
        };
        match data {
            MetaValue::Null => {}
            MetaValue::Array(array) => {
                let rendered = array
                    .elements
                    .iter()
                    .map(|element| array_element_value(&dependencies.rendering.render(element)))
                    .collect::<Vec<_>>();
                for text in rendered {
                    if matches_regex(value, &text, "annotation-data.value")? {
                        return Ok(true);
                    }
                }
            }
            _ => {
                if matches_regex(value, &dependencies.rendering.render(data), "annotation-data.value")? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Extracts the first `(value=` tail, then truncates at its first `)`; neither
/// nested arrays nor parentheses in strings are parsed (input contract).
pub fn array_element_value(text: &str) -> String {
    let tail = text.split_once("(value=").map_or(text, |(_, tail)| tail);
    tail.split_once(')').map_or(tail, |(value, _)| value).to_owned()
}
