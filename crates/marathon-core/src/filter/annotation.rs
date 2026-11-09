//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/AnnotationDataFilter.kt

use super::{FilterDependencies, single::matches_regex};
use crate::{
    error::OperationResult,
    model::{MetaValue, Test},
};
use jcompat::{JavaRegex, jnum, writers::KotlinValue};

#[derive(Default)]
pub struct AnnotationRenderer;
impl AnnotationRenderer {
    pub fn render(&self, value: &MetaValue) -> String {
        Self::value(value).render()
    }
    fn value(value: &MetaValue) -> KotlinValue {
        use MetaValue::*;
        let text = match value {
            Null => return KotlinValue::Null,
            Boolean(v) => if *v { "true" } else { "false" }.into(),
            Byte(v) => v.to_string(),
            Short(v) => v.to_string(),
            Int(v) => v.to_string(),
            Long(v) => v.to_string(),
            Char(v) => std::string::String::from_utf16_lossy(&[*v]),
            Float(v) => jnum::float_to_string(*v),
            Double(v) => jnum::double_to_string(*v),
            String(v) => v.clone(),
            List(values) => return KotlinValue::List(values.iter().map(Self::value).collect()),
            Map(map) => {
                return KotlinValue::Map(
                    map.entries()
                        .iter()
                        .map(|(key, value)| (KotlinValue::Text(key.clone()), Self::value(value)))
                        .collect(),
                );
            }
            Decoded(wrapper) => return wrapper.kotlin_value(),
            Array(array) => return KotlinValue::List(array.elements.iter().map(Self::value).collect()),
        };
        KotlinValue::Text(text)
    }
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

pub fn array_element_value(text: &str) -> String {
    let tail = text.split_once("(value=").map_or(text, |(_, tail)| tail);
    tail.split_once(')').map_or(tail, |(value, _)| value).to_owned()
}
