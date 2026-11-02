//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/SingleValueTestFilter.kt
//! and {SimpleClassnameFilter,SimpleTestnameFilter,FullyQualifiedClassnameFilter,
//! FullyQualifiedTestnameFilter,TestMethodFilter,TestPackageFilter,AnnotationFilter}.kt.

use super::{failure, select, values::FilterValuesReader};
use crate::{error::OperationResult, model::Test};
use jcompat::{JavaRegex, writers::JavaString};
use marathon_config::schema::{RegexValuesFileFilter, TestPackageFilter};
use std::sync::{Arc, Mutex};

type Values = Option<Arc<[JavaString]>>;
/// Cache locks are taken only inside the joined blocking task, never on core.
#[derive(Clone)]
pub struct SingleValueFilter {
    config: RegexValuesFileFilter,
    subpackages: bool,
    cache: Arc<Mutex<Option<Values>>>,
    reader: Arc<dyn FilterValuesReader>,
}
pub(super) enum Field {
    Class,
    SimpleTest,
    FullClass,
    FullTest,
    Method,
    Package,
    Annotation,
}
impl SingleValueFilter {
    pub(super) fn new(config: &RegexValuesFileFilter, reader: Arc<dyn FilterValuesReader>) -> Self {
        Self {
            config: config.clone(),
            subpackages: false,
            cache: Arc::default(),
            reader,
        }
    }
    pub(super) fn package(config: &TestPackageFilter, reader: Arc<dyn FilterValuesReader>) -> Self {
        Self {
            subpackages: config.subpackages,
            ..Self::new(
                &RegexValuesFileFilter {
                    regex: config.regex.clone(),
                    values: config.values.clone(),
                    file: config.file.clone(),
                    enabled: config.enabled,
                },
                reader,
            )
        }
    }
    fn values(&self) -> OperationResult<Values> {
        if let Some(values) = &self.config.values {
            return Ok(Some(values.clone().into()));
        }
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| failure("java.lang.IllegalStateException", "filter values cache poisoned"))?;
        if let Some(values) = &*cache {
            return Ok(values.clone());
        }
        let values = self
            .config
            .file
            .as_ref()
            .map(|file| self.reader.read_values(file))
            .transpose()?
            .map(Arc::from);
        *cache = Some(values.clone());
        Ok(values)
    }
    pub(super) fn select(&self, tests: &[Test], negate: bool, field: Field) -> OperationResult<Vec<Test>> {
        if !self.config.enabled || tests.is_empty() {
            return Ok(tests.to_vec());
        }
        let values = self.values()?;
        select(tests, negate, |test| {
            if matches!(field, Field::Annotation) {
                if let Some(regex) = &self.config.regex {
                    for property in &test.meta_properties {
                        if matches_regex(regex, &property.name, "annotation.name")? {
                            return Ok(true);
                        }
                    }
                    return Ok(false);
                }
                return Ok(values
                    .as_ref()
                    .is_none_or(|values| test.meta_properties.iter().any(|p| values.contains(&p.name))));
            }
            let name = match field {
                Field::Class => test.clazz.clone(),
                Field::SimpleTest => test.to_simple_safe_test_name('#'),
                Field::FullClass => test.to_class_name('.'),
                Field::FullTest => test.to_test_name('.', '#'),
                Field::Method => test.method.clone(),
                Field::Package => test.pkg.clone(),
                Field::Annotation => unreachable!(),
            };
            if let Some(regex) = &self.config.regex
                && !matches_regex(regex, &name, "test filter name")?
            {
                return Ok(false);
            }
            Ok(values.as_ref().is_none_or(|values| {
                values.iter().any(|value| {
                    if *value == name {
                        return true;
                    }
                    if self.subpackages && matches!(field, Field::Package) {
                        let mut prefix = value.clone();
                        prefix.push_str(".");
                        name.0.starts_with(&prefix.0)
                    } else {
                        false
                    }
                })
            }))
        })
    }
}

pub(super) fn matches_regex(regex: &JavaRegex, text: &JavaString, field: &str) -> OperationResult<bool> {
    let text = text.as_utf8().map_err(|_| {
        failure(
            "java.lang.IllegalArgumentException",
            &format!("invalid UTF-16 in {field} for regex {}", regex.pattern()),
        )
    })?;
    Ok(regex.matches(&text))
}
