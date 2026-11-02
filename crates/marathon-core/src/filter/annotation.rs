//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/AnnotationDataFilter.kt

use super::{FilterDependencies, failure, single::matches_regex};
use crate::{
    error::OperationResult,
    model::{MetaArray, MetaValue, Test},
    time::{ObjectIdentity, ReplayHooks},
};
use jcompat::{
    JavaRegex, jnum,
    writers::{JavaString, KotlinValue},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

/// Share this run-local renderer with consumers of metadata identity strings.
/// Tokens are allocated internally; native addresses never become output.
pub struct AnnotationRenderer {
    pub hooks: Arc<dyn ReplayHooks>,
    pub decoded_null_identity: ObjectIdentity,
    arrays: Mutex<ArrayIdentities>,
    allocate_identity: Arc<dyn Fn() -> ObjectIdentity + Send + Sync>,
}
/// Array identities keyed by address. The `Weak` detects a freed address
/// reused by a new array, and dead entries are pruned as the map grows.
#[derive(Default)]
struct ArrayIdentities {
    by_address: HashMap<usize, (Weak<MetaArray>, ObjectIdentity)>,
    prune_at: usize,
}
impl ArrayIdentities {
    fn identity(&mut self, array: &Arc<MetaArray>, allocate: &dyn Fn() -> ObjectIdentity) -> ObjectIdentity {
        let address = Arc::as_ptr(array) as usize;
        if let Some((weak, id)) = self.by_address.get(&address)
            && weak.strong_count() > 0
        {
            return *id;
        }
        if self.by_address.len() >= self.prune_at {
            self.by_address.retain(|_, (weak, _)| weak.strong_count() > 0);
            self.prune_at = (self.by_address.len() * 2).max(64);
        }
        let id = allocate();
        self.by_address.insert(address, (Arc::downgrade(array), id));
        id
    }
}
impl AnnotationRenderer {
    /// `allocate_identity` must use the run's token allocator, including tokens
    /// used by other replay consumers. Share this renderer across filter trees.
    pub fn new(
        hooks: Arc<dyn ReplayHooks>,
        decoded_null_identity: ObjectIdentity,
        allocate_identity: Arc<dyn Fn() -> ObjectIdentity + Send + Sync>,
    ) -> Self {
        Self {
            hooks,
            decoded_null_identity,
            arrays: Mutex::default(),
            allocate_identity,
        }
    }
    pub fn render(&self, value: &MetaValue) -> OperationResult<JavaString> {
        Ok(self.value(value)?.render())
    }
    fn suffix(&self, label: &'static str, identity: ObjectIdentity) -> OperationResult<JavaString> {
        self.hooks.identity_suffix(label, identity).map_err(|e| {
            failure(
                "java.lang.IllegalStateException",
                &format!("{}: {}", e.label.to_string_lossy(), e.message.to_string_lossy()),
            )
            .into()
        })
    }
    fn value(&self, value: &MetaValue) -> OperationResult<KotlinValue> {
        use MetaValue::*;
        let text = match value {
            Null => return Ok(KotlinValue::Null),
            Boolean(v) => if *v { "true" } else { "false" }.into(),
            Byte(v) => v.to_string().into(),
            Short(v) => v.to_string().into(),
            Int(v) => v.to_string().into(),
            Long(v) => v.to_string().into(),
            Char(v) => JavaString(vec![*v]),
            Float(v) => jnum::float_to_string(*v).into(),
            Double(v) => jnum::double_to_string(*v).into(),
            String(v) => v.clone(),
            List(values) => {
                return Ok(KotlinValue::List(
                    values.iter().map(|v| self.value(v)).collect::<OperationResult<_>>()?,
                ));
            }
            Map(map) => {
                return Ok(KotlinValue::Map(
                    map.entries()
                        .iter()
                        .map(|(key, value)| Ok((KotlinValue::Text(key.clone()), self.value(value)?)))
                        .collect::<OperationResult<_>>()?,
                ));
            }
            Decoded(wrapper) => {
                // Avoid consuming the identity replay tape for ordinary wrappers.
                fn needs_null(v: &crate::model::DecodedWrapper) -> bool {
                    use crate::model::DecodedWrapper::*;
                    match v {
                        DecodedNull => true,
                        DecodedArrayValue(v) => v.iter().any(needs_null),
                        _ => false,
                    }
                }
                let suffix = if needs_null(wrapper) {
                    self.suffix("metadata.decoded_null.identity", self.decoded_null_identity)?
                } else {
                    JavaString::default()
                };
                return Ok(wrapper.kotlin_value(&suffix));
            }
            Array(array) => {
                let identity = self
                    .arrays
                    .lock()
                    .map_err(|_| failure("java.lang.IllegalStateException", "metadata identity registry poisoned"))?
                    .identity(array, &*self.allocate_identity);
                let mut text = array.jvm_class.clone();
                text.push_str("@");
                text.append(self.suffix("metadata.array.identity", identity)?);
                text
            }
        };
        Ok(KotlinValue::Text(text))
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
        let Some((_, data)) = property
            .values
            .entries()
            .iter()
            .find(|(k, _)| *k == JavaString::from("value"))
        else {
            continue;
        };
        match data {
            MetaValue::Null => {}
            MetaValue::Array(array) => {
                // Kotlin maps the whole array before any(regex): even a
                // later unmatched element consumes its identity rendering.
                let rendered = array
                    .elements
                    .iter()
                    .map(|element| {
                        dependencies
                            .rendering
                            .render(element)
                            .map(|text| array_element_value(&text))
                    })
                    .collect::<OperationResult<Vec<_>>>()?;
                for text in rendered {
                    if matches_regex(value, &text, "annotation-data.value")? {
                        return Ok(true);
                    }
                }
            }
            _ => {
                if matches_regex(value, &dependencies.rendering.render(data)?, "annotation-data.value")? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

pub fn array_element_value(text: &JavaString) -> JavaString {
    let delimiter: Vec<u16> = "(value=".encode_utf16().collect();
    let start = text
        .0
        .windows(delimiter.len())
        .position(|w| w == delimiter)
        .map_or(0, |i| i + delimiter.len());
    let tail = &text.0[start..];
    let end = tail.iter().position(|u| *u == u16::from(b')')).unwrap_or(tail.len());
    JavaString(tail[..end].to_vec())
}
