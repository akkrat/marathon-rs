//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/TestFilter.kt
//! and core/src/main/kotlin/com/malinskiy/marathon/execution/filter/{CompositionFilter,FragmentationFilter}.kt
//! plus extension/ConfigurationExtensions.kt:62–75.
//!
//! Construct once per Kotlin `toTestFilter` call. Clones share lazy state, just
//! as references to the same Kotlin filter do. Await each operation to completion;
//! dropping its future detaches blocking work. No caller cancellation is inferred
//! for Kotlin's synchronous loops; injected service cancellations remain typed.

use marathon_error::FailureKind;
pub mod allure;
mod annotation;
pub use annotation::{AnnotationRenderer, array_element_value};
mod single;
pub mod values;

use crate::{
    error::{Failure, OperationError, OperationResult, VendorError},
    model::Test,
    runtime::{ServiceFuture, VendorRuntime},
};
use allure::{AllureFilter, AllurePlanSupplier};
use jcompat::{JavaRegex, fragment_hash};
use marathon_config::schema::{CompositionOperation, TestFilterConfiguration};
pub use single::SingleValueFilter;
use std::{collections::HashSet, sync::Arc};
use values::FilterValuesReader;

/// Services must remain the same for the lifetime of a filter and its clones.
#[derive(Clone)]
pub struct FilterDependencies {
    pub values: Arc<dyn FilterValuesReader>,
    pub allure: Arc<dyn AllurePlanSupplier>,
    pub rendering: Arc<AnnotationRenderer>,
}

#[derive(Clone)]
pub enum TestFilter {
    SimpleClassname(SingleValueFilter),
    SimpleTestname(SingleValueFilter),
    FullyQualifiedClassname(SingleValueFilter),
    FullyQualifiedTestname(SingleValueFilter),
    TestMethod(SingleValueFilter),
    TestPackage(SingleValueFilter),
    Annotation(SingleValueFilter),
    AnnotationData {
        name_regex: JavaRegex,
        value_regex: JavaRegex,
        enabled: bool,
        dependencies: FilterDependencies,
    },
    Fragmentation {
        index: i32,
        count: i32,
        enabled: bool,
    },
    Composition {
        filters: Vec<TestFilter>,
        op: CompositionOperation,
        enabled: bool,
    },
    Allure(AllureFilter),
}

impl TestFilter {
    /// Logical validation belongs to configuration stage (b), not this factory.
    pub fn from_config(config: &TestFilterConfiguration, dependencies: &FilterDependencies) -> Self {
        use TestFilterConfiguration as C;
        let single = |c| SingleValueFilter::new(c, dependencies.values.clone());
        match config {
            C::SimpleClassname(c) => Self::SimpleClassname(single(c)),
            C::SimpleTestname(c) => Self::SimpleTestname(single(c)),
            C::FullyQualifiedClassname(c) => Self::FullyQualifiedClassname(single(c)),
            C::FullyQualifiedTestname(c) => Self::FullyQualifiedTestname(single(c)),
            C::TestMethod(c) => Self::TestMethod(single(c)),
            C::Annotation(c) => Self::Annotation(single(c)),
            C::TestPackage(c) => Self::TestPackage(SingleValueFilter::package(c, dependencies.values.clone())),
            C::AnnotationData {
                name_regex,
                value_regex,
                enabled,
            } => Self::AnnotationData {
                name_regex: name_regex.clone(),
                value_regex: value_regex.clone(),
                enabled: *enabled,
                dependencies: dependencies.clone(),
            },
            C::Fragmentation { index, count, enabled } => Self::Fragmentation {
                index: *index,
                count: *count,
                enabled: *enabled,
            },
            C::Composition { filters, op, enabled } => Self::Composition {
                filters: filters.iter().map(|c| Self::from_config(c, dependencies)).collect(),
                op: *op,
                enabled: *enabled,
            },
            C::Allure { enabled } => Self::Allure(AllureFilter::new(*enabled, dependencies.allure.clone())),
        }
    }

    pub fn filter<'a>(&self, tests: Vec<Test>, vendor: &'a dyn VendorRuntime) -> ServiceFuture<'a, Vec<Test>> {
        self.dispatch(tests, false, vendor)
    }
    pub fn filter_not<'a>(&self, tests: Vec<Test>, vendor: &'a dyn VendorRuntime) -> ServiceFuture<'a, Vec<Test>> {
        self.dispatch(tests, true, vendor)
    }
    fn dispatch<'a>(
        &self,
        tests: Vec<Test>,
        negate: bool,
        vendor: &'a dyn VendorRuntime,
    ) -> ServiceFuture<'a, Vec<Test>> {
        let filter = self.clone();
        Box::pin(async move {
            let (send, receive) = tokio::sync::oneshot::channel();
            let task = vendor.spawn_blocking(Box::new(move || {
                let result = filter.evaluate(&tests, negate);
                let _ = send.send(result);
                Ok(())
            }));
            task.join().await.map_err(|error| match error {
                VendorError::Cancelled(c) => OperationError::Cancelled(c),
                other => OperationError::Failed(other.failure().clone()),
            })?;
            receive
                .await
                .map_err(|_| failure(FailureKind::IllegalState, "filter task finished without a result"))?
        })
    }

    fn evaluate(&self, tests: &[Test], negate: bool) -> OperationResult<Vec<Test>> {
        use TestFilter::*;
        match self {
            SimpleClassname(f) => f.select(tests, negate, single::Field::Class),
            SimpleTestname(f) => f.select(tests, negate, single::Field::SimpleTest),
            FullyQualifiedClassname(f) => f.select(tests, negate, single::Field::FullClass),
            FullyQualifiedTestname(f) => f.select(tests, negate, single::Field::FullTest),
            TestMethod(f) => f.select(tests, negate, single::Field::Method),
            TestPackage(f) => f.select(tests, negate, single::Field::Package),
            Annotation(f) => f.select(tests, negate, single::Field::Annotation),
            AnnotationData {
                name_regex,
                value_regex,
                enabled,
                dependencies,
            } => {
                if !enabled {
                    return Ok(tests.to_vec());
                }
                select(tests, negate, |test| {
                    annotation::matches(test, name_regex, value_regex, dependencies)
                })
            }
            Fragmentation { index, count, enabled } => {
                if !enabled {
                    return Ok(tests.to_vec());
                }
                select(tests, negate, |test| {
                    let hash = fragment_hash::signed_md5(&test.to_test_name('.', '#'));
                    let actual = fragment_hash::fragment_index_for_hash(hash, *count)
                        .ok_or_else(|| failure(FailureKind::Arithmetic, "BigInteger divide by zero"))?;
                    Ok(actual == i64::from(*index))
                })
            }
            Composition { filters, op, enabled } => {
                if !enabled {
                    return Ok(tests.to_vec());
                }
                let filtered = composition(tests, filters, *op)?;
                if negate {
                    Ok(difference(tests, &filtered))
                } else {
                    Ok(filtered)
                }
            }
            Allure(f) => f.select(tests, negate),
        }
    }
}

fn select(
    tests: &[Test],
    negate: bool,
    mut predicate: impl FnMut(&Test) -> OperationResult<bool>,
) -> OperationResult<Vec<Test>> {
    let mut result = Vec::new();
    for test in tests {
        if predicate(test)? != negate {
            result.push(test.clone());
        }
    }
    Ok(result)
}

// Hashing is only membership; representatives and traversal always come from
// the ordered left sequence, as Kotlin's LinkedHashSet operations do.
fn unique(tests: &[Test]) -> Vec<Test> {
    let mut seen = HashSet::new();
    tests.iter().filter(|t| seen.insert(*t)).cloned().collect()
}
fn difference(left: &[Test], right: &[Test]) -> Vec<Test> {
    let right: HashSet<_> = right.iter().collect();
    unique(left).into_iter().filter(|t| !right.contains(t)).collect()
}
fn composition(tests: &[Test], filters: &[TestFilter], op: CompositionOperation) -> OperationResult<Vec<Test>> {
    match op {
        CompositionOperation::Union => {
            let mut result = Vec::new();
            let mut seen = HashSet::new();
            for filter in filters {
                for test in filter.evaluate(tests, false)? {
                    if seen.insert(test.clone()) {
                        result.push(test);
                    }
                }
            }
            Ok(result)
        }
        CompositionOperation::Intersection => {
            let mut result = unique(tests);
            for filter in filters {
                let selected: HashSet<_> = filter.evaluate(tests, false)?.into_iter().collect();
                result.retain(|t| selected.contains(t));
            }
            Ok(result)
        }
        CompositionOperation::Subtract if filters.len() < 2 => Ok(tests.to_vec()),
        CompositionOperation::Subtract => {
            let left = filters[0].evaluate(tests, false)?;
            let mut result = unique(&left);
            for filter in &filters[1..] {
                result = difference(&result, &filter.evaluate(&left, false)?);
            }
            Ok(result)
        }
    }
}

pub(super) fn failure(class: FailureKind, message: &str) -> Failure {
    Failure {
        kind: class,
        message: Some(message.into()),
        cause: None,
        stacktrace: None,
    }
}
